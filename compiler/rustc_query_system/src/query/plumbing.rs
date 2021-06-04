//! The implementation of the query system itself. This defines the macros that
//! generate the actual methods on tcx which find and execute the provider,
//! manage the caches, and so forth.

use crate::dep_graph::{DepContext, DepKind, DepNode};
use crate::dep_graph::{DepNodeIndex, SerializedDepNodeIndex};
use crate::query::caches::QueryCache;
use crate::query::config::{QueryDescription, QueryVtable, QueryVtableExt};
use crate::query::job::{
    report_cycle, QueryInfo, QueryJob, QueryJobId, QueryJobInfo, QueryShardJobId,
};
use crate::query::{QueryContext, QueryMap, QueryStackFrame};

use rustc_data_structures::fingerprint::Fingerprint;
use rustc_data_structures::fx::{FxHashMap, FxHasher};
use rustc_data_structures::sharded::{get_shard_index_by_hash, Sharded};
use rustc_data_structures::sync::{Lock, LockGuard};
use rustc_data_structures::thin_vec::ThinVec;
#[cfg(not(parallel_compiler))]
use rustc_errors::DiagnosticBuilder;
use rustc_errors::{Diagnostic, FatalError};
use rustc_span::Span;
use std::collections::hash_map::Entry;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::mem;
use std::num::NonZeroU32;
use std::ptr;
#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct QueryCacheStore<C: QueryCache> {
    cache: C,
    shards: Sharded<C::Sharded>,
    #[cfg(debug_assertions)]
    pub cache_hits: AtomicUsize,
}

impl<C: QueryCache + Default> Default for QueryCacheStore<C> {
    fn default() -> Self {
        Self {
            cache: C::default(),
            shards: Default::default(),
            #[cfg(debug_assertions)]
            cache_hits: AtomicUsize::new(0),
        }
    }
}

// 添加注释: 检查查询缓存时使用的值, 可在缓存未命中时重用以执行查询.
/// Values used when checking a query cache which can be reused on a cache-miss to execute the query.
pub struct QueryLookup {
    pub(super) key_hash: u64,
    shard: usize,
}

// 添加注释: 我们计算一次键的散列, 然后将其用于分片查询和散列图查找. 这依赖于他们都使用
// `FxHasher`的事实.
// We compute the key's hash once and then use it for both the
// shard lookup and the hashmap lookup. This relies on the fact
// that both of them use `FxHasher`.
fn hash_for_shard<K: Hash>(key: &K) -> u64 {
    let mut hasher = FxHasher::default();
    key.hash(&mut hasher);
    hasher.finish()
}

impl<C: QueryCache> QueryCacheStore<C> {
    pub(super) fn get_lookup<'tcx>(
        &'tcx self,
        key: &C::Key,
    ) -> (QueryLookup, LockGuard<'tcx, C::Sharded>) {
        let key_hash = hash_for_shard(key);
        let shard = get_shard_index_by_hash(key_hash);
        let lock = self.shards.get_shard_by_index(shard).lock();
        (QueryLookup { key_hash, shard }, lock)
    }

    pub fn iter_results(&self, f: &mut dyn FnMut(&C::Key, &C::Value, DepNodeIndex)) {
        self.cache.iter(&self.shards, f)
    }
}

struct QueryStateShard<D, K> {
    active: FxHashMap<K, QueryResult<D>>,

    /// Used to generate unique ids for active jobs.
    jobs: u32,
}

impl<D, K> Default for QueryStateShard<D, K> {
    fn default() -> QueryStateShard<D, K> {
        QueryStateShard { active: Default::default(), jobs: 0 }
    }
}

pub struct QueryState<D, K> {
    shards: Sharded<QueryStateShard<D, K>>,
}

// 添加注释: 指示查询映射中给定键的查询状态.
/// Indicates the state of a query for a given key in a query map.
enum QueryResult<D> {
    // 添加注释: 一个已经在执行的查询. 查询作业可用于等待其完成.
    /// An already executing query. The query job can be used to await for its completion.
    Started(QueryJob<D>),

    // 添加注释: 查询是`panic`. 试图等待这个查询将引发一个致命的错误, 它会默默的`panic`.
    /// The query panicked. Queries trying to wait on this will raise a fatal error which will
    /// silently panic.
    Poisoned,
}

impl<D, K> QueryState<D, K>
where
    D: Copy + Clone + Eq + Hash,
    K: Eq + Hash + Clone + Debug,
{
    pub fn all_inactive(&self) -> bool {
        let shards = self.shards.lock_shards();
        shards.iter().all(|shard| shard.active.is_empty())
    }

    pub fn try_collect_active_jobs<CTX: Copy>(
        &self,
        tcx: CTX,
        kind: D,
        make_query: fn(CTX, K) -> QueryStackFrame,
        jobs: &mut QueryMap<D>,
    ) -> Option<()> {
        // We use try_lock_shards here since we are called from the
        // deadlock handler, and this shouldn't be locked.
        let shards = self.shards.try_lock_shards()?;
        for (shard_id, shard) in shards.iter().enumerate() {
            for (k, v) in shard.active.iter() {
                if let QueryResult::Started(ref job) = *v {
                    let id = QueryJobId::new(job.id, shard_id, kind);
                    let info = QueryInfo { span: job.span, query: make_query(tcx, k.clone()) };
                    jobs.insert(id, QueryJobInfo { info, job: job.clone() });
                }
            }
        }

        Some(())
    }
}

impl<D, K> Default for QueryState<D, K> {
    fn default() -> QueryState<D, K> {
        QueryState { shards: Default::default() }
    }
}

/// A type representing the responsibility to execute the job in the `job` field.
/// This will poison the relevant query if dropped.
struct JobOwner<'tcx, D, C>
where
    D: Copy + Clone + Eq + Hash,
    C: QueryCache,
{
    state: &'tcx QueryState<D, C::Key>,
    cache: &'tcx QueryCacheStore<C>,
    key: C::Key,
    id: QueryJobId<D>,
}

#[cold]
#[inline(never)]
#[cfg(not(parallel_compiler))]
fn mk_cycle<CTX, V, R>(
    tcx: CTX,
    root: QueryJobId<CTX::DepKind>,
    span: Span,
    handle_cycle_error: fn(CTX, DiagnosticBuilder<'_>) -> V,
    cache: &dyn crate::query::QueryStorage<Value = V, Stored = R>,
) -> R
where
    CTX: QueryContext,
    V: std::fmt::Debug,
    R: Clone,
{
    let error: CycleError = root.find_cycle_in_stack(
        tcx.try_collect_active_jobs().unwrap(),
        &tcx.current_query_job(),
        span,
    );
    let error = report_cycle(tcx.dep_context().sess(), error);
    let value = handle_cycle_error(tcx, error);
    cache.store_nocache(value)
}

impl<'tcx, D, C> JobOwner<'tcx, D, C>
where
    D: Copy + Clone + Eq + Hash,
    C: QueryCache,
{
    // 添加注释: 要么获取与查询对应的`JobOwner`, 允许我们开始执行查询, 或者返回查询结果.
    // 这个函数假设`try_get_cached`已经被调用并返回`lookup`.
    // 如果查询正在其它地方执行, 它将等待它并返回结果.
    // 如果查询是`panic`, 这将默默地`panic`.
    /// Either gets a `JobOwner` corresponding the query, allowing us to
    /// start executing the query, or returns with the result of the query.
    /// This function assumes that `try_get_cached` is already called and returned `lookup`.
    /// If the query is executing elsewhere, this will wait for it and return the result.
    /// If the query panicked, this will silently panic.
    ///
    // 添加注释: 这个函数是内联的, 因为这会显着提高某些编译时基准测试的速度.
    /// This function is inlined because that results in a noticeable speed-up
    /// for some compile-time benchmarks.
    #[inline(always)]
    fn try_start<'b, CTX>(
        tcx: CTX,
        state: &'b QueryState<CTX::DepKind, C::Key>,
        cache: &'b QueryCacheStore<C>,
        span: Span,
        key: C::Key,
        lookup: QueryLookup,
        query: &QueryVtable<CTX, C::Key, C::Value>,
    ) -> TryGetJob<'b, CTX::DepKind, C>
    where
        CTX: QueryContext,
    {
        let shard = lookup.shard;
        let mut state_lock = state.shards.get_shard_by_index(shard).lock();
        let lock = &mut *state_lock;

        match lock.active.entry(key) {
            // 添加注释: 根据`key`获取没有获取到条目
            Entry::Vacant(entry) => {
                // 添加注释: 在此分片中生成唯一的id
                // Generate an id unique within this shard.
                let id = lock.jobs.checked_add(1).unwrap();
                lock.jobs = id;
                let id = QueryShardJobId(NonZeroU32::new(id).unwrap());

                // 添加注释: 从TLS上下文中获取查询信息
                let job = tcx.current_query_job();
                // 添加注释: 创建一个新的查询job
                let job = QueryJob::new(id, span, job);

                let key = entry.key().clone();
                // 添加注释: `entry.insert`将使用VacantEntry的键设置条目的值, 并返回对它的可变引用
                entry.insert(QueryResult::Started(job));

                let global_id = QueryJobId::new(id, shard, query.dep_kind);
                let owner = JobOwner { state, cache, id: global_id, key };
                return TryGetJob::NotYetStarted(owner);
            }
            // 添加注释: 根据`key`获取到了条目
            Entry::Occupied(mut entry) => {
                match entry.get_mut() {
                    #[cfg(not(parallel_compiler))]
                    QueryResult::Started(job) => {
                        let id = QueryJobId::new(job.id, shard, query.dep_kind);

                        drop(state_lock);

                        // 添加注释: 如果我们是单线程的, 我们知道我们有循环错误, 所以我们只返回错误.
                        // If we are single-threaded we know that we have cycle error,
                        // so we just return the error.
                        return TryGetJob::Cycle(mk_cycle(
                            tcx,
                            id,
                            span,
                            query.handle_cycle_error,
                            &cache.cache,
                        ));
                    }
                    #[cfg(parallel_compiler)]
                    QueryResult::Started(job) => {
                        // For parallel queries, we'll block and wait until the query running
                        // in another thread has completed. Record how long we wait in the
                        // self-profiler.
                        let query_blocked_prof_timer = tcx.dep_context().profiler().query_blocked();

                        // Get the latch out
                        let latch = job.latch();
                        let key = entry.key().clone();

                        drop(state_lock);

                        // With parallel queries we might just have to wait on some other
                        // thread.
                        let result = latch.wait_on(tcx.current_query_job(), span);

                        if let Err(cycle) = result {
                            let cycle = report_cycle(tcx.dep_context().sess(), cycle);
                            let value = (query.handle_cycle_error)(tcx, cycle);
                            let value = cache.cache.store_nocache(value);
                            return TryGetJob::Cycle(value);
                        }

                        let cached = cache
                            .cache
                            .lookup(cache, &key, |value, index| {
                                if unlikely!(tcx.dep_context().profiler().enabled()) {
                                    tcx.dep_context().profiler().query_cache_hit(index.into());
                                }
                                #[cfg(debug_assertions)]
                                {
                                    cache.cache_hits.fetch_add(1, Ordering::Relaxed);
                                }
                                (value.clone(), index)
                            })
                            .unwrap_or_else(|_| panic!("value must be in cache after waiting"));

                        query_blocked_prof_timer.finish_with_query_invocation_id(cached.1.into());

                        return TryGetJob::JobCompleted(cached);
                    }
                    QueryResult::Poisoned => FatalError.raise(),
                }
            }
        }
    }

    // 添加注释: 通过使用`result`更新查询缓存来完成查询, 向`waiter`发出信号并忘记JobOwner,
    // 因此它不会poison查询
    /// Completes the query by updating the query cache with the `result`,
    /// signals the waiter and forgets the JobOwner, so it won't poison the query
    fn complete(self, result: C::Value, dep_node_index: DepNodeIndex) -> C::Stored {
        // We can move out of `self` here because we `mem::forget` it below
        let key = unsafe { ptr::read(&self.key) };
        let state = self.state;
        let cache = self.cache;

        // 添加注释: 忘记`self`, 这样`destructor`就不会`poison`查询
        // Forget ourself so our destructor won't poison the query
        mem::forget(self);

        let (job, result) = {
            let key_hash = hash_for_shard(&key);
            let shard = get_shard_index_by_hash(key_hash);
            let job = {
                let mut lock = state.shards.get_shard_by_index(shard).lock();
                match lock.active.remove(&key).unwrap() {
                    QueryResult::Started(job) => job,
                    QueryResult::Poisoned => panic!(),
                }
            };
            let result = {
                let mut lock = cache.shards.get_shard_by_index(shard).lock();
                cache.cache.complete(&mut lock, key, result, dep_node_index)
            };
            (job, result)
        };

        // 添加注释: 非并行下不会有任何操作
        job.signal_complete();
        result
    }
}

fn with_diagnostics<F, R>(f: F) -> (R, ThinVec<Diagnostic>)
where
    F: FnOnce(Option<&Lock<ThinVec<Diagnostic>>>) -> R,
{
    let diagnostics = Lock::new(ThinVec::new());
    let result = f(Some(&diagnostics));
    (result, diagnostics.into_inner())
}

impl<'tcx, D, C> Drop for JobOwner<'tcx, D, C>
where
    D: Copy + Clone + Eq + Hash,
    C: QueryCache,
{
    #[inline(never)]
    #[cold]
    fn drop(&mut self) {
        // Poison the query so jobs waiting on it panic.
        let state = self.state;
        let shard = state.shards.get_shard_by_value(&self.key);
        let job = {
            let mut shard = shard.lock();
            let job = match shard.active.remove(&self.key).unwrap() {
                QueryResult::Started(job) => job,
                QueryResult::Poisoned => panic!(),
            };
            shard.active.insert(self.key.clone(), QueryResult::Poisoned);
            job
        };
        // Also signal the completion of the job, so waiters
        // will continue execution.
        job.signal_complete();
    }
}

#[derive(Clone)]
pub(crate) struct CycleError {
    /// The query and related span that uses the cycle.
    pub usage: Option<(Span, QueryStackFrame)>,
    pub cycle: Vec<QueryInfo>,
}

// 添加注释: `try_start`的结果
/// The result of `try_start`.
enum TryGetJob<'tcx, D, C>
where
    D: Copy + Clone + Eq + Hash,
    C: QueryCache,
{
    // 添加注释: 查询尚未开始. 包含对最终用于启动它的缓存的保护.
    /// The query is not yet started. Contains a guard to the cache eventually used to start it.
    NotYetStarted(JobOwner<'tcx, D, C>),

    /// The query was already completed.
    /// Returns the result of the query and its dep-node index
    /// if it succeeded or a cycle error if it failed.
    #[cfg(parallel_compiler)]
    JobCompleted((C::Stored, DepNodeIndex)),

    // 添加注释: 尝试执行查询导致循环.
    /// Trying to execute the query resulted in a cycle.
    Cycle(C::Stored),
}

// 添加注释: 检查查询是否已经计算并在缓存中. 它将分片索引和锁保护返加给分片, 如果
// 查询不在缓存中并且我们需要计算它, 它将被使用.
/// Checks if the query is already computed and in the cache.
/// It returns the shard index and a lock guard to the shard,
/// which will be used if the query is not in the cache and we need
/// to compute it.
#[inline]
pub fn try_get_cached<'a, CTX, C, R, OnHit>(
    tcx: CTX,
    cache: &'a QueryCacheStore<C>,
    key: &C::Key,
    // 添加注释: `on_hit`可以在持有查询缓存锁时调用
    // `on_hit` can be called while holding a lock to the query cache
    on_hit: OnHit,
) -> Result<R, QueryLookup>
where
    C: QueryCache,
    CTX: DepContext,
    OnHit: FnOnce(&C::Stored) -> R,
{
    cache.cache.lookup(cache, &key, |value, index| {
        if unlikely!(tcx.profiler().enabled()) {
            tcx.profiler().query_cache_hit(index.into());
        }
        #[cfg(debug_assertions)]
        {
            cache.cache_hits.fetch_add(1, Ordering::Relaxed);
        }
        tcx.dep_graph().read_index(index);
        on_hit(value)
    })
}

fn try_execute_query<CTX, C>(
    tcx: CTX,
    state: &QueryState<CTX::DepKind, C::Key>,
    cache: &QueryCacheStore<C>,
    span: Span,
    key: C::Key,
    lookup: QueryLookup,
    query: &QueryVtable<CTX, C::Key, C::Value>,
) -> C::Stored
where
    C: QueryCache,
    C::Key: crate::dep_graph::DepNodeParams<CTX::DepContext>,
    CTX: QueryContext,
{
    // 添加注释: 调用`try_start`函数, 如果返回`TryGetJob::Cycle`类型时则直接return
    let job = match JobOwner::<'_, CTX::DepKind, C>::try_start(
        tcx,
        state,
        cache,
        span,
        key.clone(),
        lookup,
        query,
    ) {
        TryGetJob::NotYetStarted(job) => job,
        TryGetJob::Cycle(result) => return result,
        #[cfg(parallel_compiler)]
        TryGetJob::JobCompleted((v, index)) => {
            tcx.dep_context().dep_graph().read_index(index);
            return v;
        }
    };

    // 添加注释: 当`incr.`, `comp.`关闭时的快速路径. `to_dep_node`对于某些`DepKind`来说是昂贵的.
    // Fast path for when incr. comp. is off. `to_dep_node` is
    // expensive for some `DepKind`s.
    // 添加注释: `is_fully_enabled`表示如果我们正在构建完整的深度图, 则返回`true`, 否则返回`false`
    if !tcx.dep_context().dep_graph().is_fully_enabled() {
        let null_dep_node = DepNode::new_no_params(DepKind::NULL);
        return force_query_with_job(tcx, key, job, null_dep_node, query).0;
    }

    if query.anon {
        let prof_timer = tcx.dep_context().profiler().query_provider();

        let ((result, dep_node_index), diagnostics) = with_diagnostics(|diagnostics| {
            tcx.start_query(job.id, diagnostics, || {
                tcx.dep_context().dep_graph().with_anon_task(
                    *tcx.dep_context(),
                    query.dep_kind,
                    || query.compute(tcx, key),
                )
            })
        });

        prof_timer.finish_with_query_invocation_id(dep_node_index.into());

        tcx.dep_context().dep_graph().read_index(dep_node_index);

        if unlikely!(!diagnostics.is_empty()) {
            tcx.store_diagnostics_for_anon_node(dep_node_index, diagnostics);
        }

        return job.complete(result, dep_node_index);
    }

    let dep_node = query.to_dep_node(*tcx.dep_context(), &key);

    if !query.eval_always {
        // 添加注释: 此查询的诊断将在`try_mark_green()`期间提升到当前会话, 因此我们可以在此处
        // 忽略它们.
        // The diagnostics for this query will be
        // promoted to the current session during
        // `try_mark_green()`, so we can ignore them here.
        let loaded = tcx.start_query(job.id, None, || {
            let marked = tcx.dep_context().dep_graph().try_mark_green_and_read(tcx, &dep_node);
            marked.map(|(prev_dep_node_index, dep_node_index)| {
                (
                    load_from_disk_and_cache_in_memory(
                        tcx,
                        key.clone(),
                        prev_dep_node_index,
                        dep_node_index,
                        &dep_node,
                        query,
                    ),
                    dep_node_index,
                )
            })
        });
        if let Some((result, dep_node_index)) = loaded {
            return job.complete(result, dep_node_index);
        }
    }

    let (result, dep_node_index) = force_query_with_job(tcx, key, job, dep_node, query);
    tcx.dep_context().dep_graph().read_index(dep_node_index);
    result
}

fn load_from_disk_and_cache_in_memory<CTX, K, V: Debug>(
    tcx: CTX,
    key: K,
    prev_dep_node_index: SerializedDepNodeIndex,
    dep_node_index: DepNodeIndex,
    dep_node: &DepNode<CTX::DepKind>,
    query: &QueryVtable<CTX, K, V>,
) -> V
where
    CTX: QueryContext,
{
    // Note this function can be called concurrently from the same query
    // We must ensure that this is handled correctly.

    debug_assert!(tcx.dep_context().dep_graph().is_green(dep_node));

    // First we try to load the result from the on-disk cache.
    let result = if query.cache_on_disk(tcx, &key, None) {
        let prof_timer = tcx.dep_context().profiler().incr_cache_loading();
        let result = query.try_load_from_disk(tcx, prev_dep_node_index);
        prof_timer.finish_with_query_invocation_id(dep_node_index.into());

        // We always expect to find a cached result for things that
        // can be forced from `DepNode`.
        debug_assert!(
            !dep_node.kind.can_reconstruct_query_key() || result.is_some(),
            "missing on-disk cache entry for {:?}",
            dep_node
        );
        result
    } else {
        // Some things are never cached on disk.
        None
    };

    if let Some(result) = result {
        // If `-Zincremental-verify-ich` is specified, re-hash results from
        // the cache and make sure that they have the expected fingerprint.
        if unlikely!(tcx.dep_context().sess().opts.debugging_opts.incremental_verify_ich) {
            incremental_verify_ich(*tcx.dep_context(), &result, dep_node, query);
        }

        result
    } else {
        // We could not load a result from the on-disk cache, so
        // recompute.
        let prof_timer = tcx.dep_context().profiler().query_provider();

        // The dep-graph for this computation is already in-place.
        let result = tcx.dep_context().dep_graph().with_ignore(|| query.compute(tcx, key));

        prof_timer.finish_with_query_invocation_id(dep_node_index.into());

        // Verify that re-running the query produced a result with the expected hash
        // This catches bugs in query implementations, turning them into ICEs.
        // For example, a query might sort its result by `DefId` - since `DefId`s are
        // not stable across compilation sessions, the result could get up getting sorted
        // in a different order when the query is re-run, even though all of the inputs
        // (e.g. `DefPathHash` values) were green.
        //
        // See issue #82920 for an example of a miscompilation that would get turned into
        // an ICE by this check
        incremental_verify_ich(*tcx.dep_context(), &result, dep_node, query);

        result
    }
}

fn incremental_verify_ich<CTX, K, V: Debug>(
    tcx: CTX::DepContext,
    result: &V,
    dep_node: &DepNode<CTX::DepKind>,
    query: &QueryVtable<CTX, K, V>,
) where
    CTX: QueryContext,
{
    assert!(
        tcx.dep_graph().is_green(dep_node),
        "fingerprint for green query instance not loaded from cache: {:?}",
        dep_node,
    );

    debug!("BEGIN verify_ich({:?})", dep_node);
    let mut hcx = tcx.create_stable_hashing_context();

    let new_hash = query.hash_result(&mut hcx, result).unwrap_or(Fingerprint::ZERO);
    debug!("END verify_ich({:?})", dep_node);

    let old_hash = tcx.dep_graph().prev_fingerprint_of(dep_node);

    assert_eq!(
        Some(new_hash),
        old_hash,
        "found unstable fingerprints for {:?}: {:?}",
        dep_node,
        result
    );
}

// 添加注释: 强制查询job
fn force_query_with_job<C, CTX>(
    tcx: CTX,
    key: C::Key,
    job: JobOwner<'_, CTX::DepKind, C>,
    dep_node: DepNode<CTX::DepKind>,
    query: &QueryVtable<CTX, C::Key, C::Value>,
) -> (C::Stored, DepNodeIndex)
where
    C: QueryCache,
    CTX: QueryContext,
{
    // 添加注释: 如果下面的断言触发, 可能有两个原因:
    //           1. DepNode创建有问题, 无论是在此处还是在`DepGraph::try_mark_green()`中.
    //           2. 两个不同的查询键被映射到同一个`DepNode`(参见示例#48923).
    // If the following assertion triggers, it can have two reasons:
    // 1. Something is wrong with DepNode creation, either here or
    //    in `DepGraph::try_mark_green()`.
    // 2. Two distinct query keys get mapped to the same `DepNode`
    //    (see for example #48923).
    assert!(
        !tcx.dep_context().dep_graph().dep_node_exists(&dep_node),
        "forcing query with already existing `DepNode`\n\
                 - query-key: {:?}\n\
                 - dep-node: {:?}",
        key,
        dep_node
    );

    let prof_timer = tcx.dep_context().profiler().query_provider();

    let ((result, dep_node_index), diagnostics) = with_diagnostics(|diagnostics| {
        tcx.start_query(job.id, diagnostics, || {
            // 添加注释: 根据query.eval_always来判断走哪个分支
            if query.eval_always {
                tcx.dep_context().dep_graph().with_eval_always_task(
                    dep_node,
                    tcx,
                    key,
                    query.compute,
                    query.hash_result,
                )
            } else {
                tcx.dep_context().dep_graph().with_task(
                    dep_node,
                    tcx,
                    key,
                    query.compute,
                    query.hash_result,
                )
            }
        })
    });

    prof_timer.finish_with_query_invocation_id(dep_node_index.into());

    if unlikely!(!diagnostics.is_empty()) && dep_node.kind != DepKind::NULL {
        tcx.store_diagnostics(dep_node_index, diagnostics);
    }

    let result = job.complete(result, dep_node_index);

    (result, dep_node_index)
}

#[inline(never)]
fn get_query_impl<CTX, C>(
    tcx: CTX,
    state: &QueryState<CTX::DepKind, C::Key>,
    cache: &QueryCacheStore<C>,
    span: Span,
    key: C::Key,
    lookup: QueryLookup,
    query: &QueryVtable<CTX, C::Key, C::Value>,
) -> C::Stored
where
    CTX: QueryContext,
    C: QueryCache,
    C::Key: crate::dep_graph::DepNodeParams<CTX::DepContext>,
{
    try_execute_query(tcx, state, cache, span, key, lookup, query)
}

// 添加注释: 确保此查询具有所有绿色输入或已执行.
// 执行`query::ensure(D)`被认为是对dep-node `D`的读取.
// 如果查询仍应运行, 则返回true.

// 添加注释: 这个函数在执行副作用传递时特别有用 --例如, 为了报告错误程序的错误.
/// Ensure that either this query has all green inputs or been executed.
/// Executing `query::ensure(D)` is considered a read of the dep-node `D`.
/// Returns true if the query should still run.
///
/// This function is particularly useful when executing passes for their
/// side-effects -- e.g., in order to report errors for erroneous programs.
///
// 添加注释: 优化仅在incr期间可用.
/// Note: The optimization is only available during incr. comp.
#[inline(never)]
fn ensure_must_run<CTX, K, V>(tcx: CTX, key: &K, query: &QueryVtable<CTX, K, V>) -> bool
where
    K: crate::dep_graph::DepNodeParams<CTX::DepContext>,
    CTX: QueryContext,
{
    if query.eval_always {
        return true;
    }

    // 添加注释: 确保匿名查询毫无意义
    // Ensuring an anonymous query makes no sense
    assert!(!query.anon);

    let dep_node = query.to_dep_node(*tcx.dep_context(), key);

    match tcx.dep_context().dep_graph().try_mark_green_and_read(tcx, &dep_node) {
        None => {
            // 添加注释: 从`try_mark_green_and_read`返回None意味着这是一个新的dep节点或者
            // dep节点已经被标记为红色. 无论哪种方式, 我们都不能调用`dep_graph.read()`, 因为我们
            // 没有DepNodeIndex. 我们必须调用查询本身. 这引入的性能成本应该可以忽略不计, 因为我们将
            // 立即访问内存缓存, 或者下一个查询.
            // A None return from `try_mark_green_and_read` means that this is either
            // a new dep node or that the dep node has already been marked red.
            // Either way, we can't call `dep_graph.read()` as we don't have the
            // DepNodeIndex. We must invoke the query itself. The performance cost
            // this introduces should be negligible as we'll immediately hit the
            // in-memory cache, or another query down the line will.
            true
        }
        Some((_, dep_node_index)) => {
            tcx.dep_context().profiler().query_cache_hit(dep_node_index.into());
            false
        }
    }
}

#[inline(never)]
fn force_query_impl<CTX, C>(
    tcx: CTX,
    state: &QueryState<CTX::DepKind, C::Key>,
    cache: &QueryCacheStore<C>,
    key: C::Key,
    span: Span,
    dep_node: DepNode<CTX::DepKind>,
    query: &QueryVtable<CTX, C::Key, C::Value>,
) where
    C: QueryCache,
    C::Key: crate::dep_graph::DepNodeParams<CTX::DepContext>,
    CTX: QueryContext,
{
    // We may be concurrently trying both execute and force a query.
    // Ensure that only one of them runs the query.
    let cached = cache.cache.lookup(cache, &key, |_, index| {
        if unlikely!(tcx.dep_context().profiler().enabled()) {
            tcx.dep_context().profiler().query_cache_hit(index.into());
        }
        #[cfg(debug_assertions)]
        {
            cache.cache_hits.fetch_add(1, Ordering::Relaxed);
        }
    });

    let lookup = match cached {
        Ok(()) => return,
        Err(lookup) => lookup,
    };

    let job = match JobOwner::<'_, CTX::DepKind, C>::try_start(
        tcx,
        state,
        cache,
        span,
        key.clone(),
        lookup,
        query,
    ) {
        TryGetJob::NotYetStarted(job) => job,
        TryGetJob::Cycle(_) => return,
        #[cfg(parallel_compiler)]
        TryGetJob::JobCompleted(_) => return,
    };
    force_query_with_job(tcx, key, job, dep_node, query);
}

pub enum QueryMode {
    Get,
    Ensure,
}

pub fn get_query<Q, CTX>(
    tcx: CTX,
    span: Span,
    key: Q::Key,
    lookup: QueryLookup,
    mode: QueryMode,
) -> Option<Q::Stored>
where
    Q: QueryDescription<CTX>,
    Q::Key: crate::dep_graph::DepNodeParams<CTX::DepContext>,
    CTX: QueryContext,
{
    let query = &Q::VTABLE;
    // 添加注释: 判断查询模式
    if let QueryMode::Ensure = mode {
        if !ensure_must_run(tcx, &key, query) {
            return None;
        }
    }

    debug!("ty::query::get_query<{}>(key={:?}, span={:?})", Q::NAME, key, span);
    let value =
        get_query_impl(tcx, Q::query_state(tcx), Q::query_cache(tcx), span, key, lookup, query);
    Some(value)
}

pub fn force_query<Q, CTX>(tcx: CTX, key: Q::Key, span: Span, dep_node: DepNode<CTX::DepKind>)
where
    Q: QueryDescription<CTX>,
    Q::Key: crate::dep_graph::DepNodeParams<CTX::DepContext>,
    CTX: QueryContext,
{
    force_query_impl(tcx, Q::query_state(tcx), Q::query_cache(tcx), key, span, dep_node, &Q::VTABLE)
}

use proc_macro::TokenStream;
use proc_macro2::{Delimiter, TokenTree};
use quote::quote;
use syn::parse::{Parse, ParseStream, Result};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    braced, parenthesized, parse_macro_input, parse_quote, AttrStyle, Attribute, Block, Error,
    Expr, Ident, ReturnType, Token, Type,
};

mod kw {
    syn::custom_keyword!(query);
}

// 添加注释: 标识或通配符`_`
/// Ident or a wildcard `_`.
struct IdentOrWild(Ident);

impl Parse for IdentOrWild {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        Ok(if input.peek(Token![_]) {
            // 添加注释: 如果第一个token是`_`, 则解析为`_`
            let underscore = input.parse::<Token![_]>()?;
            // 添加注释: 将`_`转换为`Ident`
            IdentOrWild(Ident::new("_", underscore.span()))
        } else {
            // 添加注释: 如果第一个token不是`_`则将解析为`Ident`
            IdentOrWild(input.parse()?)
        })
    }
}

// 添加注释: 查询的修饰符
/// A modifier for a query
enum QueryModifier {
    // 添加注释: 查询的描述
    /// The description of the query.
    Desc(Option<Ident>, Punctuated<Expr, Token![,]>),

    // 添加注释: 将此类型用于内存中缓存
    /// Use this type for the in-memory cache.
    Storage(Type),

    // 添加注释: 如果`Expr`返回true, 则将查询缓存到磁盘
    /// Cache the query to disk if the `Expr` returns true.
    Cache(Option<(IdentOrWild, IdentOrWild)>, Block),

    // 添加注释: 自定义代码以从磁盘加载查询
    /// Custom code to load the query from disk.
    LoadCached(Ident, Ident, Block),

    // 添加注释: 该查询的循环错误中止了致命错误, 使编译中止
    /// A cycle error for this query aborting the compilation with a fatal error.
    FatalCycle,

    // 添加注释: 循环错误导致delay_bug调用
    /// A cycle error results in a delay_bug call
    CycleDelayBug,

    // 添加注释: 不要对结果进行hash处理, 而是在查询运行时将其标记为红色
    /// Don't hash the result, instead just mark a query red if it runs
    NoHash,

    // 添加注释: 根据查询的依赖关系生成一个dep节点
    /// Generate a dep node based on the dependencies of the query
    Anon,

    // 添加注释: 始终评估查询, 而忽略其依赖性
    /// Always evaluate the query, ignoring its dependencies
    EvalAlways,
}

impl Parse for QueryModifier {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        // 添加注释: 获取标识符
        let modifier: Ident = input.parse()?;
        if modifier == "desc" {
            // Parse a description modifier like:
            // `desc { |tcx| "foo {}", tcx.item_path(key) }`
            let attr_content;
            braced!(attr_content in input);
            let tcx = if attr_content.peek(Token![|]) {
                attr_content.parse::<Token![|]>()?;
                let tcx = attr_content.parse()?;
                attr_content.parse::<Token![|]>()?;
                Some(tcx)
            } else {
                None
            };
            // 添加注释: `parse_terminated<T, P: Parse>`函数将解析零次或更多次出现的T(由P类型的标点符号分隔)以及可选
            // 的尾随标点符号.
            // 解析将一直持续到解析流结束为止. 该解析流的全部内容必须由T和P组成.
            let desc = attr_content.parse_terminated(Expr::parse)?;
            Ok(QueryModifier::Desc(tcx, desc))
        } else if modifier == "cache_on_disk_if" {
            // Parse a cache modifier like:
            // `cache(tcx, value) { |tcx| key.is_local() }`
            // 添加注释: `TokenTree::Group`代表由括号定界符包围的令牌流
            let has_args = if let TokenTree::Group(group) = input.fork().parse()? {
                // 添加注释: 判断`group.delimiter()`是否等于`( ... )`, 当条件成功则代表有参数
                group.delimiter() == Delimiter::Parenthesis
            } else {
                false
            };
            let args = if has_args {
                let args;
                // 添加注释: `parenthesized!`宏将解析一组括号, 并将其内容提供给后续的解析器
                parenthesized!(args in input);
                // 添加注释: 以下代码说明令牌流在使用后游标将向下移动, 即当解析当前类型后则会向下移动
                let tcx = args.parse()?;
                // 添加注释: 当`args.parse::<Token![,]>()?`执行时, 如果当前token不为`,`时将返回Err, 而在使用?语法糖时将直接return
                args.parse::<Token![,]>()?;
                let value = args.parse()?;
                Some((tcx, value))
            } else {
                None
            };
            // 添加注释: 包含Rust语句的支撑块
            let block = input.parse()?;
            Ok(QueryModifier::Cache(args, block))
        } else if modifier == "load_cached" {
            // Parse a load_cached modifier like:
            // `load_cached(tcx, id) { tcx.on_disk_cache.try_load_query_result(tcx, id) }`
            let args;
            parenthesized!(args in input);
            let tcx = args.parse()?;
            args.parse::<Token![,]>()?;
            let id = args.parse()?;
            let block = input.parse()?;
            Ok(QueryModifier::LoadCached(tcx, id, block))
        } else if modifier == "storage" {
            let args;
            parenthesized!(args in input);
            let ty = args.parse()?;
            Ok(QueryModifier::Storage(ty))
        } else if modifier == "fatal_cycle" {
            Ok(QueryModifier::FatalCycle)
        } else if modifier == "cycle_delay_bug" {
            Ok(QueryModifier::CycleDelayBug)
        } else if modifier == "no_hash" {
            Ok(QueryModifier::NoHash)
        } else if modifier == "anon" {
            Ok(QueryModifier::Anon)
        } else if modifier == "eval_always" {
            Ok(QueryModifier::EvalAlways)
        } else {
            Err(Error::new(modifier.span(), "unknown query modifier"))
        }
    }
}

// 添加注释: 确保仅使用文档注释属性
/// Ensures only doc comment attributes are used
fn check_attributes(attrs: Vec<Attribute>) -> Result<Vec<Attribute>> {
    let inner = |attr: Attribute| {
        if !attr.path.is_ident("doc") {
            // 添加注释: 只支持doc
            Err(Error::new(attr.span(), "attributes not supported on queries"))
        } else if attr.style != AttrStyle::Outer {
            // 添加注释: 属性必须是外部属性(`///`), 而不是内部属性.
            Err(Error::new(
                attr.span(),
                "attributes must be outer attributes (`///`), not inner attributes",
            ))
        } else {
            Ok(attr)
        }
    };
    // 添加注释: 当前函数最终返回的类型是Result<Vec<Attribute>>
    // 则最终调用的collect()函数是调用的Result类型实现的FromIterator trait中的`from_iter`方法
    attrs.into_iter().map(inner).collect()
}

/// A compiler query. `query ... { ... }`
struct Query {
    doc_comments: Vec<Attribute>,
    modifiers: List<QueryModifier>,
    name: Ident,
    key: IdentOrWild,
    arg: Type,
    result: ReturnType,
}

impl Parse for Query {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let doc_comments = check_attributes(input.call(Attribute::parse_outer)?)?;

        // 添加注释: 解析查询声明. 像`query type_of(key: DefId) -> Ty<'tcx>`
        // Parse the query declaration. Like `query type_of(key: DefId) -> Ty<'tcx>`
        input.parse::<kw::query>()?;
        // 添加注释: 获取Ident(标识符)
        let name: Ident = input.parse()?;
        let arg_content;
        // 添加注释: `parenthesized!`宏表示将解析一组括号, 并将其内容提供给后续的解析器.
        parenthesized!(arg_content in input);
        let key = arg_content.parse()?;
        arg_content.parse::<Token![:]>()?;
        // 添加注释: 解析参数类型
        let arg = arg_content.parse()?;
        // 添加注释: 获取返回类型
        let result = input.parse()?;

        // 添加注释: 解析查询修饰符, 此处声明的`content`将会在`braced!`宏内进行赋值
        // Parse the query modifiers
        let content;
        // 添加注释: `braced!`宏用于解析一组花括号, 并将其内容显示给后续的解析器
        braced!(content in input);
        // 添加注释: 解析查询修饰符
        let modifiers = content.parse()?;

        Ok(Query { doc_comments, modifiers, name, key, arg, result })
    }
}

// 添加注释: 一种用于贪婪地解析另一种类型, 直到输入为空的类型
/// A type used to greedily parse another type until the input is empty.
struct List<T>(Vec<T>);

impl<T: Parse> Parse for List<T> {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let mut list = Vec::new();
        while !input.is_empty() {
            // 添加注释: 由泛型参数定义义list内存储的元素的类型, 将调用对应泛型类型实现的parse方法
            list.push(input.parse()?);
        }
        Ok(List(list))
    }
}

struct QueryModifiers {
    /// The description of the query.
    desc: (Option<Ident>, Punctuated<Expr, Token![,]>),

    /// Use this type for the in-memory cache.
    storage: Option<Type>,

    /// Cache the query to disk if the `Block` returns true.
    cache: Option<(Option<(IdentOrWild, IdentOrWild)>, Block)>,

    /// Custom code to load the query from disk.
    load_cached: Option<(Ident, Ident, Block)>,

    /// A cycle error for this query aborting the compilation with a fatal error.
    fatal_cycle: bool,

    /// A cycle error results in a delay_bug call
    cycle_delay_bug: bool,

    /// Don't hash the result, instead just mark a query red if it runs
    no_hash: bool,

    /// Generate a dep node based on the dependencies of the query
    anon: bool,

    // Always evaluate the query, ignoring its dependencies
    eval_always: bool,
}

// 添加注释: 将查询修饰符处理为结构, 对重复项进行错误处理
/// Process query modifiers into a struct, erroring on duplicates
fn process_modifiers(query: &mut Query) -> QueryModifiers {
    let mut load_cached = None;
    let mut storage = None;
    let mut cache = None;
    let mut desc = None;
    let mut fatal_cycle = false;
    let mut cycle_delay_bug = false;
    let mut no_hash = false;
    let mut anon = false;
    let mut eval_always = false;
    for modifier in query.modifiers.0.drain(..) {
        match modifier {
            QueryModifier::LoadCached(tcx, id, block) => {
                if load_cached.is_some() {
                    // 添加注释: 重复定义校验, 当出现重复时将panic
                    panic!("duplicate modifier `load_cached` for query `{}`", query.name);
                }
                load_cached = Some((tcx, id, block));
            }
            QueryModifier::Storage(ty) => {
                if storage.is_some() {
                    panic!("duplicate modifier `storage` for query `{}`", query.name);
                }
                storage = Some(ty);
            }
            QueryModifier::Cache(args, expr) => {
                if cache.is_some() {
                    panic!("duplicate modifier `cache` for query `{}`", query.name);
                }
                cache = Some((args, expr));
            }
            QueryModifier::Desc(tcx, list) => {
                if desc.is_some() {
                    panic!("duplicate modifier `desc` for query `{}`", query.name);
                }
                // 添加注释: 如果没有文档注释, 请通过显示查询描述至少了解其作用.
                // If there are no doc-comments, give at least some idea of what
                // it does by showing the query description.
                if query.doc_comments.is_empty() {
                    use ::syn::*;
                    let mut list = list.iter();
                    // 添加注释: 读取`list`迭代器中的第一个元素
                    let format_str: String = match list.next() {
                        // 添加注释: 获取`desc { .. }`中的String表达式
                        Some(&Expr::Lit(ExprLit { lit: Lit::Str(ref lit_str), .. })) => {
                            lit_str.value().replace("`{}`", "{}") // We add them later anyways for consistency
                        }
                        _ => panic!("Expected a string literal"),
                    };
                    let mut fmt_fragments = format_str.split("{}");
                    let mut doc_string = fmt_fragments.next().unwrap().to_string();
                    // 添加注释: `::quote::ToTokens::to_token_stream`函数将self直接转换为TokenStream对象.
                    // 此方法使用to_tokens隐式实现, 并且充当ToTokens trait的使用者的快捷方法
                    list.map(::quote::ToTokens::to_token_stream).zip(fmt_fragments).for_each(
                        |(tts, next_fmt_fragment)| {
                            use ::core::fmt::Write;
                            write!(
                                &mut doc_string,
                                " `{}` {}",
                                tts.to_string().replace(" . ", "."),
                                next_fmt_fragment,
                            )
                            .unwrap();
                        },
                    );
                    let doc_string = format!(
                        "[query description - consider adding a doc-comment!] {}",
                        doc_string
                    );
                    // 添加注释: `parse_quote!`宏可以像`quote!`宏一样接收输入, 但
                    // 使用类型推断来找出这些标记的返回类型
                    let comment = parse_quote! {
                        #[doc = #doc_string]
                    };
                    query.doc_comments.push(comment);
                }
                desc = Some((tcx, list));
            }
            QueryModifier::FatalCycle => {
                if fatal_cycle {
                    panic!("duplicate modifier `fatal_cycle` for query `{}`", query.name);
                }
                fatal_cycle = true;
            }
            QueryModifier::CycleDelayBug => {
                if cycle_delay_bug {
                    panic!("duplicate modifier `cycle_delay_bug` for query `{}`", query.name);
                }
                cycle_delay_bug = true;
            }
            QueryModifier::NoHash => {
                if no_hash {
                    panic!("duplicate modifier `no_hash` for query `{}`", query.name);
                }
                no_hash = true;
            }
            QueryModifier::Anon => {
                if anon {
                    panic!("duplicate modifier `anon` for query `{}`", query.name);
                }
                anon = true;
            }
            QueryModifier::EvalAlways => {
                if eval_always {
                    panic!("duplicate modifier `eval_always` for query `{}`", query.name);
                }
                eval_always = true;
            }
        }
    }
    let desc = desc.unwrap_or_else(|| {
        panic!("no description provided for query `{}`", query.name);
    });
    QueryModifiers {
        load_cached,
        storage,
        cache,
        desc,
        fatal_cycle,
        cycle_delay_bug,
        no_hash,
        anon,
        eval_always,
    }
}

// 添加注释: 如果需要, 将查询的QueryDescription的impl添加到`impls`
/// Add the impl of QueryDescription for the query to `impls` if one is requested
fn add_query_description_impl(
    query: &Query,
    modifiers: QueryModifiers,
    impls: &mut proc_macro2::TokenStream,
) {
    let name = &query.name;
    let key = &query.key.0;

    // 添加注释: 找出我们是否应该将查询缓存在磁盘上
    // Find out if we should cache the query on disk
    let cache = if let Some((args, expr)) = modifiers.cache.as_ref() {
        let try_load_from_disk = if let Some((tcx, id, block)) = modifiers.load_cached.as_ref() {
            // 添加注释: 使用自定义代码从磁盘加载查询
            // Use custom code to load the query from disk
            quote! {
                #[inline]
                fn try_load_from_disk(
                    #tcx: QueryCtxt<'tcx>,
                    #id: SerializedDepNodeIndex
                ) -> Option<Self::Value> {
                    #block
                }
            }
        } else {
            // 添加注释: 使用默认代码从磁盘加载查询
            // Use the default code to load the query from disk
            quote! {
                #[inline]
                fn try_load_from_disk(
                    tcx: QueryCtxt<'tcx>,
                    id: SerializedDepNodeIndex
                ) -> Option<Self::Value> {
                    tcx.on_disk_cache.as_ref()?.try_load_query_result(*tcx, id)
                }
            }
        };

        let tcx = args
            .as_ref()
            .map(|t| {
                let t = &(t.0).0;
                quote! { #t }
            })
            .unwrap_or_else(|| quote! { _ });
        let value = args
            .as_ref()
            .map(|t| {
                let t = &(t.1).0;
                quote! { #t }
            })
            .unwrap_or_else(|| quote! { _ });
        // expr is a `Block`, meaning that `{ #expr }` gets expanded
        // to `{ { stmts... } }`, which triggers the `unused_braces` lint.
        quote! {
            #[inline]
            #[allow(unused_variables, unused_braces)]
            fn cache_on_disk(
                #tcx: QueryCtxt<'tcx>,
                #key: &Self::Key,
                #value: Option<&Self::Value>
            ) -> bool {
                #expr
            }

            #try_load_from_disk
        }
    } else {
        if modifiers.load_cached.is_some() {
            panic!("load_cached modifier on query `{}` without a cache modifier", name);
        }
        quote! {}
    };

    let (tcx, desc) = modifiers.desc;
    let tcx = tcx.as_ref().map_or_else(|| quote! { _ }, |t| quote! { #t });

    let desc = quote! {
        #[allow(unused_variables)]
        fn describe(tcx: QueryCtxt<'tcx>, key: Self::Key) -> String {
            let (#tcx, #key) = (*tcx, key);
            ::rustc_middle::ty::print::with_no_trimmed_paths(|| format!(#desc).into())
        }
    };

    // 添加注释: 为生成的代码实现`QueryDescription<QueryCtxt<'tcx>>`
    impls.extend(quote! {
        impl<'tcx> QueryDescription<QueryCtxt<'tcx>> for queries::#name<'tcx> {
            #desc
            #cache
        }
    });
}

pub fn rustc_queries(input: TokenStream) -> TokenStream {
    // 添加注释: 将输入的TokenStream转换为List<Query>类型结构体变量,
    // 此处要求List<Query>必须要实现syn::parse::Parse trait
    let queries = parse_macro_input!(input as List<Query>);

    let mut query_stream = quote! {};
    let mut query_description_stream = quote! {};
    let mut dep_node_def_stream = quote! {};
    let mut cached_queries = quote! {};

    // 添加注释: 遍历输入的TokenStream
    for mut query in queries.0 {
        // 添加注释: 获取修饰符的处理结果
        let modifiers = process_modifiers(&mut query);
        let name = &query.name;
        let arg = &query.arg;
        let result_full = &query.result;
        let result = match query.result {
            ReturnType::Default => quote! { -> () },
            _ => quote! { #result_full },
        };

        if modifiers.cache.is_some() {
            cached_queries.extend(quote! {
                #name,
            });
        }

        let mut attributes = Vec::new();

        // Pass on the fatal_cycle modifier
        if modifiers.fatal_cycle {
            attributes.push(quote! { fatal_cycle });
        };
        // Pass on the storage modifier
        if let Some(ref ty) = modifiers.storage {
            attributes.push(quote! { storage(#ty) });
        };
        // Pass on the cycle_delay_bug modifier
        if modifiers.cycle_delay_bug {
            attributes.push(quote! { cycle_delay_bug });
        };
        // Pass on the no_hash modifier
        if modifiers.no_hash {
            attributes.push(quote! { no_hash });
        };
        // Pass on the anon modifier
        if modifiers.anon {
            attributes.push(quote! { anon });
        };
        // Pass on the eval_always modifier
        if modifiers.eval_always {
            attributes.push(quote! { eval_always });
        };

        let attribute_stream = quote! {#(#attributes),*};
        let doc_comments = query.doc_comments.iter();
        // Add the query to the group
        query_stream.extend(quote! {
            #(#doc_comments)*
            [#attribute_stream] fn #name(#arg) #result,
        });

        // Create a dep node for the query
        dep_node_def_stream.extend(quote! {
            [#attribute_stream] #name(#arg),
        });

        add_query_description_impl(&query, modifiers, &mut query_description_stream);
    }

    // 添加注释: 生成最终代码, 生成了四个宏! .....
    TokenStream::from(quote! {
        #[macro_export]
        macro_rules! rustc_query_append {
            ([$($macro:tt)*][$($other:tt)*]) => {
                $($macro)* {
                    $($other)*

                    #query_stream

                }
            }
        }
        macro_rules! rustc_dep_node_append {
            ([$($macro:tt)*][$($other:tt)*]) => {
                $($macro)*(
                    $($other)*

                    #dep_node_def_stream
                );
            }
        }
        #[macro_export]
        macro_rules! rustc_cached_queries {
            ($($macro:tt)*) => {
                $($macro)*(#cached_queries);
            }
        }
        #[macro_export]
        macro_rules! rustc_query_description {
            () => { #query_description_stream }
        }
    })
}

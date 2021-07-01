// 添加注释: rustbuild, Rust构建系统
//! rustbuild, the Rust build system
//!
// 添加注释: 这是用于编译`rustc`编译器的构建系统的入口点. 可以在父目录的`README.md`文件中找到大量文档,
// 否则可以在每个相应模块的`build`目录中找到文档
//! This is the entry point for the build system used to compile the `rustc`
//! compiler. Lots of documentation can be found in the `README.md` file in the
//! parent directory, and otherwise documentation can be found throughout the `build`
//! directory in each respective module.

use std::env;

use bootstrap::{Build, Config, Subcommand, VERSION};

fn main() {
    // 添加注释: 跳过第一个元素获取环境变量集合
    let args = env::args().skip(1).collect::<Vec<_>>();
    let config = Config::parse(&args);

    // 添加注释: 安装过程中不打印 check_version警告
    // check_version warnings are not printed during setup
    let changelog_suggestion =
        if matches!(config.cmd, Subcommand::Setup { .. }) { None } else { check_version(&config) };

    // 添加注释: 注意: 由于`./configure`生成`config.toml`, 发行版维护者将看到更改日志警告, 而不是`x.py`消息.
    // NOTE: Since `./configure` generates a `config.toml`, distro maintainers will see the
    // changelog warning, not the `x.py setup` message.
    let suggest_setup = !config.config.exists() && !matches!(config.cmd, Subcommand::Setup { .. });
    if suggest_setup {
        println!("warning: you have not made a `config.toml`");
        println!(
            "help: consider running `./x.py setup` or copying `config.toml.example` by running \
            `cp config.toml.example config.toml`"
        );
    } else if let Some(suggestion) = &changelog_suggestion {
        println!("{}", suggestion);
    }

    Build::new(config).build();

    if suggest_setup {
        // 添加注释: 警告: 你还没有创建`config.toml`
        println!("warning: you have not made a `config.toml`");
        println!(
            "help: consider running `./x.py setup` or copying `config.toml.example` by running \
            `cp config.toml.example config.toml`"
        );
    } else if let Some(suggestion) = &changelog_suggestion {
        println!("{}", suggestion);
    }

    if suggest_setup || changelog_suggestion.is_some() {
        // 添加注释: 此消息已打印两次以使其更有可能被看到
        println!("note: this message was printed twice to make it more likely to be seen");
    }
}

fn check_version(config: &Config) -> Option<String> {
    let mut msg = String::new();

    let suggestion = if let Some(seen) = config.changelog_seen {
        if seen != VERSION {
            // 添加注释: 警告: 自上次更新以来x.py发生了变化.
            msg.push_str("warning: there have been changes to x.py since you last updated.\n");
            // 添加注释: 更新`config.toml`以使用`changelog-seen = xx`
            format!("update `config.toml` to use `changelog-seen = {}` instead", VERSION)
        } else {
            // 添加注释: seen 和 VERSION相等则返回None
            return None;
        }
    } else {
        // 添加注释: 警告: x.py最近做了一些改变, 你可能想看看
        msg.push_str("warning: x.py has made several changes recently you may want to look at\n");
        // 添加注释: 在`config.toml`顶部添加`changelog-seen = xx`
        format!("add `changelog-seen = {}` at the top of `config.toml`", VERSION)
    };

    // 添加注释: 考虑查看`src/bootstrap/CHANGELOG.md`中的更改
    msg.push_str("help: consider looking at the changes in `src/bootstrap/CHANGELOG.md`\n");
    msg.push_str("note: to silence this warning, ");
    msg.push_str(&suggestion);

    Some(msg)
}

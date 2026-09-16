mod cli;
mod commands;
mod tui;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let args = cli::Cli::parse();

    // daemon run 自带文件日志,跳过全局 stdout subscriber(否则文件日志无法注册)
    let is_daemon_run = matches!(&args.cmd, Some(cli::Cmd::Daemon { cmd: cli::DaemonCmd::Run }));
    if !is_daemon_run {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .init();
    }
    match args.cmd {
        None | Some(cli::Cmd::Ui) => tui::run(),
        Some(cli::Cmd::Serve { addr, gate, data_dir }) => {
            commands::serve::run(addr.as_deref(), gate.as_deref(), data_dir)
        }
        Some(cli::Cmd::Ping { url, data_dir }) => commands::ping::run(url.as_deref(), data_dir.as_deref()),
        Some(cli::Cmd::Login { url, token }) => commands::login::run(&url, &token),
        Some(cli::Cmd::Set { key, value, ttl, content_type }) => {
            commands::kv::set(&key, value, ttl.as_deref(), &content_type)
        }
        Some(cli::Cmd::Get { key, meta }) => commands::kv::get(&key, meta),
        Some(cli::Cmd::Ls { prefix, json, limit }) => commands::kv::ls(prefix.as_deref(), json, limit),
        Some(cli::Cmd::Rm { key }) => commands::kv::rm(&key),
        Some(cli::Cmd::Watch { prefix, since_version }) => {
            commands::kv::watch(prefix.as_deref(), since_version)
        }
        Some(cli::Cmd::Daemon { cmd }) => commands::daemon::dispatch(cmd),
    }
}

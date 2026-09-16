use anyhow::Result;

use crate::cli::DaemonCmd;

pub fn dispatch(cmd: DaemonCmd) -> Result<()> {
    match cmd {
        DaemonCmd::Run => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(airctx_client::daemon::run())
        }
        DaemonCmd::Start => {
            airctx_client::daemon::start()?;
            println!("daemon started");
            Ok(())
        }
        DaemonCmd::Stop => {
            airctx_client::daemon::stop()?;
            println!("daemon stopped");
            Ok(())
        }
        DaemonCmd::Status => airctx_client::daemon::status_cli(),
        DaemonCmd::Sync => {
            airctx_client::daemon::sync_now()?;
            println!("sync triggered");
            Ok(())
        }
        DaemonCmd::Install => {
            airctx_client::daemon::install()?;
            println!("autostart enabled");
            Ok(())
        }
        DaemonCmd::Uninstall => {
            airctx_client::daemon::uninstall()?;
            println!("autostart disabled");
            Ok(())
        }
    }
}

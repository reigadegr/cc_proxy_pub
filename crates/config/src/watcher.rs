use std::sync::Arc;

use notify::{
    EventKind, RecursiveMode, Watcher,
    event::{AccessKind, AccessMode},
};
use tracing::{error, info};

use crate::AtomicConfig;

pub fn start_config_watcher(config: Arc<AtomicConfig>) {
    std::thread::spawn(move || {
        let config_path = config.config_path().to_path_buf();

        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) => {
                    if matches!(
                        event.kind,
                        EventKind::Access(AccessKind::Close(AccessMode::Write))
                    ) {
                        config.reload();
                    }
                }
                Err(error) => error!("Config watch error: {}", error),
            }) {
                Ok(watcher) => watcher,
                Err(error) => {
                    error!("Failed to initialize watcher: {}", error);
                    return;
                }
            };

        if let Err(error) = watcher.watch(&config_path, RecursiveMode::NonRecursive) {
            error!("Failed to add watch for config file: {}", error);
            return;
        }

        info!("👁️  配置文件监听已启动: {:?}", config_path);
        std::thread::park();
    });
}

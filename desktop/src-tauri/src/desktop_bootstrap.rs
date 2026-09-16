//! Start storage and recovery on a worker. The native event loop is available
//! immediately, including while a large profile upgrades its query indexes.
use aworkit_desktop::{
    management::{LocalRepairLedgerAdapter, ManagementRepairGateway},
    runtime::{CommittedChatEventPort, DesktopRuntime, LayoutConfigurationV2},
};
use aworkit_local_store::RedactionSet;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};
use tauri::Manager;

#[derive(Default)]
pub struct RuntimeHost {
    ready: OnceLock<Result<Arc<Mutex<DesktopRuntime>>, String>>,
}

impl RuntimeHost {
    /// Called only by blocking workers, never by a native callback.
    pub fn lock(&self) -> Result<MutexGuard<'_, DesktopRuntime>, String> {
        self.ready
            .wait()
            .as_ref()
            .map_err(Clone::clone)?
            .lock()
            .map_err(|_| "desktop runtime lock is unavailable".into())
    }
    pub fn shared(&self) -> Result<Arc<Mutex<DesktopRuntime>>, String> {
        self.ready.wait().clone()
    }
}

pub fn start(app: &tauri::AppHandle, root: PathBuf) {
    let host = Arc::new(RuntimeHost::default());
    app.manage(host.clone());
    app.manage(crate::desktop_layout::LayoutSession::new(
        LayoutConfigurationV2::default(),
    ));
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = (|| {
            let ledger = Arc::new(
                LocalRepairLedgerAdapter::for_store_root(
                    root.join("repair"),
                    RedactionSet::default(),
                )
                .map_err(|e| format!("repair ledger: {e}"))?,
            );
            let committed: Arc<dyn CommittedChatEventPort> =
                Arc::new(crate::TauriCommittedChatEvents { app: app.clone() });
            let runtime = DesktopRuntime::open_with_web_renderer(
                root.join("runtime"),
                committed,
                Arc::new(aworkit_desktop::web_renderer::NativeWebRenderer::new(
                    app.clone(),
                )),
            )?
            .with_management_repair(ManagementRepairGateway::with_durable_ledger(ledger));
            app.manage(runtime.cancellation_controller());
            app.manage(runtime.image_store());
            let layout = runtime.layout();
            app.state::<crate::desktop_layout::LayoutSession>()
                .restore(layout.clone())?;
            if let Err(error) = crate::desktop_layout::restore_window_layout(&app, &layout) {
                eprintln!("aworkit: could not restore window placement: {error}");
            }
            Ok(Arc::new(Mutex::new(runtime)))
        })();
        if let Err(error) = &result {
            eprintln!("aworkit: startup failed: {error}");
        }
        let _ = host.ready.set(result);
    });
}

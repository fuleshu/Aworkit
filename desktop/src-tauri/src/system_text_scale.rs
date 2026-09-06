//! Projects the OS accessibility text preference, separately from window DPI.
//! The Windows subscription must stay alive for the entire application lifetime.

pub struct SystemTextScale {
    #[cfg(target_os = "windows")]
    subscription: Option<(windows::UI::ViewManagement::UISettings, i64)>,
}

impl SystemTextScale {
    pub fn current(&self) -> f64 {
        // Read the native source instead of caching an event payload: a startup
        // snapshot racing with TextScaleFactorChanged must never retain stale data.
        #[cfg(target_os = "windows")]
        if let Some((settings, _)) = &self.subscription {
            return settings.TextScaleFactor().unwrap_or(1.0);
        }
        1.0
    }

    /// Display DPI remains owned by the native webview on every platform.
    pub fn observe(app: &tauri::AppHandle) -> Self {
        #[cfg(target_os = "windows")]
        let subscription = {
            use tauri::Emitter;
            use windows::{Foundation::TypedEventHandler, UI::ViewManagement::UISettings};
            let subscribe = || -> windows::core::Result<_> {
                let settings = UISettings::new()?;
                let app = app.clone();
                let token = settings.TextScaleFactorChanged(&TypedEventHandler::<
                    UISettings,
                    windows::core::IInspectable,
                >::new(
                    move |sender, _| {
                        if let Some(sender) = sender.as_ref() {
                            let scale = sender.TextScaleFactor()?;
                            let _ = app.emit("aworkit:system-text-scale", scale);
                        }
                        Ok(())
                    },
                ))?;
                Ok((settings, token))
            };
            subscribe()
                .map_err(|error| eprintln!("Windows text scaling is unavailable: {error}"))
                .ok()
        };
        #[cfg(not(target_os = "windows"))]
        let _ = app;
        Self {
            #[cfg(target_os = "windows")]
            subscription,
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for SystemTextScale {
    fn drop(&mut self) {
        if let Some((settings, token)) = &self.subscription {
            let _ = settings.RemoveTextScaleFactorChanged(*token);
        }
    }
}

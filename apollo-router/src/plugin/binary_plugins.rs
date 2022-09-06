//! Binary plugins support.

use async_ffi::FfiFuture;
use libloading::Library;
use tower::BoxError;

use crate::plugin::DynPlugin;

/// Declare a binary plugin.
#[macro_export]
macro_rules! declare_plugin {
    ($constructor:path, $conf:ty) => {
        #[no_mangle]
        pub extern "C" fn _plugin_create(
            cfg: Box<serde_json::Value>,
        ) -> FfiFuture<*mut Result<Box<dyn DynPlugin>, BoxError>> {
            // TODO: We should'nt have to define a subscriber here, but
            // we are doing because of shortcomings in the tracing
            // crate. This needs a more permanent fix
            // By default, tracing will give us a filter which filters
            // at level ERROR. That's not what we want, so if we don't
            // have a filter specification, let's create one set at
            // level INFO.
            let filter = match tracing_subscriber::filter::EnvFilter::try_from_default_env() {
                Ok(f) => f,
                Err(_e) => tracing_subscriber::filter::EnvFilter::new("info"),
            };
            // TODO: What about json()?
            let _subscriber = match tracing_subscriber::fmt::fmt()
                .with_env_filter(filter)
                .try_init()
            {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("ignoring subscriber init error in plugin: {}", e);
                }
            };
            async move {
                // We can't just shortcut error handling here...
                let pi: PluginInit<$conf> = match PluginInit::try_new(*cfg, Default::default()) {
                    Ok(v) => v,
                    Err(e) => return Box::into_raw(Box::new(Err(e))),
                };
                let my_box = match $constructor(pi).await {
                    Ok(v) => Box::new(v) as Box<dyn DynPlugin>,
                    Err(e) => return Box::into_raw(Box::new(Err(e))),
                };
                Box::into_raw(Box::new(Ok(my_box)))
            }
            .into_ffi()
        }
    };
}

// Filter out our candidate plugins by platform
#[cfg(target_os = "linux")]
fn is_shared_object(name: &str) -> bool {
    name.ends_with(".so")
}

#[cfg(target_os = "windows")]
fn is_shared_object(name: &str) -> bool {
    name.ends_with(".dll")
}

#[cfg(target_os = "macos")]
fn is_shared_object(name: &str) -> bool {
    name.ends_with(".dylib")
}

/// Create a plugin
async fn create_plugin(path: &str) -> Result<Box<dyn DynPlugin>, BoxError> {
    let plugin;
    unsafe {
        let hdl = libloading::Library::new(path)?;

        {
            let p_hdl: *const Library = &hdl;
            tracing::debug!("loaded shared object: {:p}", p_hdl);
        }
        // Find our plugin
        let ctr: libloading::Symbol<
            unsafe extern "C" fn(
                Box<serde_json::Value>,
            )
                -> FfiFuture<Box<Result<Box<dyn DynPlugin>, BoxError>>>,
        > = hdl.get(b"_plugin_create")?;
        {
            let p_ctr: *const libloading::Symbol<
                unsafe extern "C" fn(
                    Box<serde_json::Value>,
                )
                    -> FfiFuture<Box<Result<Box<dyn DynPlugin>, BoxError>>>,
            > = &ctr;
            tracing::debug!("located plugin constructor: {:p}", p_ctr);
        }
        let cfg = if path.contains("libbin_hello_world") {
            serde_json::json!({
                "name" : "Gary"
            })
        } else if path.contains("libbin_forbid_anonymous_operations") {
            serde_json::json!(null)
        } else {
            panic!("library not recognised");
        };
        // Build our plugin
        plugin = (*(ctr)(Box::new(cfg)).await)?;
    }
    Ok(plugin)
}

pub(crate) async fn scan_plugins() -> Result<Vec<(String, Box<dyn DynPlugin>)>, BoxError> {
    // let plugins_dir = std::env::args().nth(1).ok_or("no library supplied")?;
    let plugins_dir = "./target/release";
    let mut result = vec![];

    let mut entries = tokio::fs::read_dir(plugins_dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        // Check if the path name is a valid String and
        // looks like a shared object
        let is_plugin = match entry.path().to_str() {
            Some(path) => is_shared_object(path),
            None => false,
        };
        if is_plugin {
            // We know we can convert the path to a string now
            let path = entry.path().to_str().unwrap().to_string();
            tracing::debug!("creating: {}", path);
            let plugin = create_plugin(path.as_str()).await?;
            result.push((path, plugin));
        }
    }

    Ok(result)
}

use std::{collections::HashMap, sync::Arc};

use wasmtime::{Engine, Instance, Module, Store};
use wasmtime_wasi::sync::WasiCtxBuilder;

#[derive(Clone)]
pub struct Extensions {
    engine: Engine,
    modules: Arc<HashMap<String, Module>>,
    configuration: Arc<HashMap<String, Configuration>>,
}

pub struct Configuration {
    pub path: String,
    pub hook: String,
}

impl Extensions {
    pub fn new(
        configuration: HashMap<String, Configuration>,
        // FIXME: we should wrap the error correctly here
    ) -> anyhow::Result<Self> {
        // FIXME: investigate Engine configuration, especially around TLS
        let engine = Engine::default();
        let mut modules = HashMap::new();

        for (name, extension) in &configuration {
            tracing::info!("building module {} at path {}", name, extension.path);
            let module = Module::from_file(&engine, extension.path.clone())?;
            for import in module.imports() {
                tracing::info!("import: {:?}", import);
            }
            for export in module.exports() {
                tracing::info!("export: {:?}", export);
            }
            modules.insert(name.clone(), module);
        }

        Ok(Self {
            engine,
            modules: Arc::new(modules),
            configuration: Arc::new(configuration),
        })
    }

    pub fn context(&self) -> ExecutionContext {
        let store = Store::new(&self.engine, ());

        ExecutionContext {
            extensions: self.clone(),
            store,
            instances: HashMap::new(),
        }
    }

    //FIXME: we will need to be flexible on deciding where and on which condition we hook
    pub fn find(&self, hook: String) -> Option<String> {
        for (name, conf) in &*self.configuration {
            if conf.hook == hook {
                return Some(name.clone());
            }
        }
        None
    }
}

pub struct ExecutionContext {
    extensions: Extensions,
    pub store: Store<()>,
    instances: HashMap<String, Instance>,
}

impl ExecutionContext {
    pub fn instantiate(&mut self, hook: String) -> Option<Instance> {
        if let Some(instance) = self.instances.get(&hook) {
            return Some(*instance);
        }

        if let Some(name) = self.extensions.find(hook) {
            if let Some(module) = self.extensions.modules.get(&name) {
                //FIXME: error, imports
                let instance = Instance::new(&mut self.store, &module, &[]).unwrap();
                self.instances.insert(name, instance);

                return Some(instance);
            }
        }
        None
    }
}

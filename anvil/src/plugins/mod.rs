use std::path::{Path, PathBuf};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

// 1. Генерация типов из .wit
wasmtime::component::bindgen!({
    path: "wit",
    world: "window-manager-plugin",
});

// Реэкспортируем типы наружу
pub use anvil::wm::types::*;

// Для совместимости
pub type WindowId = u64;
pub type WSize = Size;

// 2. Конвертеры из Smithay в типы плагина
impl From<smithay::utils::Rectangle<i32, smithay::utils::Logical>> for Rectangle {
    fn from(rect: smithay::utils::Rectangle<i32, smithay::utils::Logical>) -> Self {
        Self {
            x: rect.loc.x,
            y: rect.loc.y,
            width: rect.size.w.max(0) as u32,
            height: rect.size.h.max(0) as u32,
        }
    }
}

impl From<smithay::utils::Point<f64, smithay::utils::Logical>> for Point {
    fn from(pt: smithay::utils::Point<f64, smithay::utils::Logical>) -> Self {
        Self {
            x: pt.x.round() as i32,
            y: pt.y.round() as i32,
        }
    }
}

// 3. Состояние для WASI-окружения
pub struct HostState {
    pub wasi_ctx: WasiCtx,
    pub table: ResourceTable,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.table,
        }
    }
}

// 4. Менеджер плагинов Wasmtime
pub struct PluginManager {
    engine: Engine,
    linker: Linker<HostState>,
    plugins_dir: PathBuf,
    loaded_plugin: Option<(Store<HostState>, WindowManagerPlugin)>,
}

impl std::fmt::Debug for PluginManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginManager")
            .field("plugins_dir", &self.plugins_dir)
            .field("is_loaded", &self.loaded_plugin.is_some())
            .finish()
    }
}

impl PluginManager {
    pub fn new(plugins_dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config)?;
        let mut linker = Linker::<HostState>::new(&engine);

        // Подключаем системные вызовы WASI p2
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;

        Ok(Self {
            engine,
            linker,
            plugins_dir: plugins_dir.to_path_buf(),
            loaded_plugin: None,
        })
    }

    pub fn list_available_plugins(&self) -> Vec<String> {
        let mut plugins = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.plugins_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("wasm") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        plugins.push(stem.to_string());
                    }
                }
            }
        }
        plugins
    }

    pub fn load_plugin(&mut self, name: &str) -> Result<(), Box<dyn std::error::Error>> {
        let wasm_path = self.plugins_dir.join(format!("{}.wasm", name));
        let component = Component::from_file(&self.engine, &wasm_path)?;

        let state = HostState {
            wasi_ctx: WasiCtx::builder().inherit_stdio().inherit_env().build(),
            table: ResourceTable::new(),
        };
        let mut store = Store::new(&self.engine, state);
        let plugin = WindowManagerPlugin::instantiate(&mut store, &component, &self.linker)?;

        self.loaded_plugin = Some((store, plugin));
        Ok(())
    }

    pub fn calculate_layout(
        &mut self,
        windows: Vec<WindowInfo>,
        context: DisplayContext,
    ) -> Result<Vec<(u64, WindowPlacement)>, Box<dyn std::error::Error>> {
        let Some((store, plugin)) = &mut self.loaded_plugin else {
            return Ok(Vec::new());
        };

        let result = plugin
            .anvil_wm_layout_engine()
            .call_calculate_layout(store, &windows, &context)?;
        Ok(result)
    }
}

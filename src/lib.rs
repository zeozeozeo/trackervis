#[cfg(target_arch = "wasm32")]
mod oscilloscope;
#[cfg(target_arch = "wasm32")]
mod render_host;
#[cfg(target_arch = "wasm32")]
mod visualizer;
#[cfg(target_arch = "wasm32")]
mod wasm_app;
#[cfg(target_arch = "wasm32")]
mod web_openmpt;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    wasm_app::start().map_err(|error| JsValue::from_str(&format!("{error:#}")))
}

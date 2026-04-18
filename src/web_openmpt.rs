use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Context, Result, anyhow};
use js_sys::{Array, Float32Array, Object, Reflect};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;

#[derive(Debug, Clone)]
pub struct DecodedTrack {
    pub label: String,
    pub filename: String,
    pub audio_sample_rate: u32,
    pub scope_sample_rate: u32,
    pub duration_seconds: f64,
    pub audio_left: Vec<f32>,
    pub audio_right: Vec<f32>,
    pub channel_samples: Vec<Vec<f32>>,
}

#[wasm_bindgen(module = "/web/audio_worker_bridge.js")]
unsafe extern "C" {
    #[wasm_bindgen(catch, js_name = decodeModuleStream)]
    async fn decode_module_stream_js(
        bytes: Vec<u8>,
        filename: String,
        sample_rate: u32,
        on_track: &js_sys::Function,
    ) -> std::result::Result<JsValue, JsValue>;
}

pub async fn decode_module_stream(
    bytes: Vec<u8>,
    filename: String,
    sample_rate: u32,
    mut on_track: impl FnMut(DecodedTrack) + 'static,
) -> Result<()> {
    let parse_error: Rc<RefCell<Option<anyhow::Error>>> = Rc::new(RefCell::new(None));
    let parse_error_cb = Rc::clone(&parse_error);
    let on_track = Closure::wrap(Box::new(move |value: JsValue| {
        if parse_error_cb.borrow().is_some() {
            return;
        }
        match parse_track(value) {
            Ok(track) => on_track(track),
            Err(error) => {
                *parse_error_cb.borrow_mut() = Some(error);
            }
        }
    }) as Box<dyn FnMut(JsValue)>);

    decode_module_stream_js(
        bytes,
        filename,
        sample_rate,
        on_track.as_ref().unchecked_ref(),
    )
    .await
    .map_err(js_error)
    .context("browser decoder failed")?;
    drop(on_track);

    if let Some(error) = parse_error.borrow_mut().take() {
        return Err(error);
    }

    Ok(())
}

fn parse_track(value: JsValue) -> Result<DecodedTrack> {
    let object = Object::from(value);
    let channels = Array::from(&get(&object, "channels")?);
    let mut channel_samples = Vec::with_capacity(channels.length() as usize);
    for channel in channels.iter() {
        channel_samples.push(float_array(channel)?);
    }

    Ok(DecodedTrack {
        label: string(&object, "label")?,
        filename: string(&object, "filename")?,
        audio_sample_rate: number(&object, "audioSampleRate")? as u32,
        scope_sample_rate: number(&object, "scopeSampleRate")? as u32,
        duration_seconds: number(&object, "durationSeconds")?,
        audio_left: float_array(get(&object, "audioLeft")?)?,
        audio_right: float_array(get(&object, "audioRight")?)?,
        channel_samples,
    })
}

fn get(object: &Object, key: &str) -> Result<JsValue> {
    let value = Reflect::get(object, &JsValue::from_str(key)).map_err(js_error)?;
    if value.is_undefined() || value.is_null() {
        Err(anyhow!("missing field `{key}`"))
    } else {
        Ok(value)
    }
}

fn string(object: &Object, key: &str) -> Result<String> {
    get(object, key)?
        .as_string()
        .ok_or_else(|| anyhow!("field `{key}` is not a string"))
}

fn number(object: &Object, key: &str) -> Result<f64> {
    get(object, key)?
        .as_f64()
        .ok_or_else(|| anyhow!("field `{key}` is not a number"))
}

fn float_array(value: JsValue) -> Result<Vec<f32>> {
    Ok(Float32Array::new(&value).to_vec())
}

fn js_error(error: JsValue) -> anyhow::Error {
    if let Some(text) = error.as_string() {
        anyhow!(text)
    } else {
        anyhow!(format!("{error:?}"))
    }
}

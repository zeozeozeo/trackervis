use anyhow::{Context, Result, anyhow};
use vello::util::{RenderContext, RenderSurface};
use vello::{RenderParams, Renderer, RendererOptions, Scene};
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::visualizer::{FrameView, aa_config, render_to_scene};

#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc;

pub struct VelloSurfaceRenderer {
    render_context: RenderContext,
    surface: RenderSurface<'static>,
    renderer: Renderer,
    scene: Scene,
    pub size: PhysicalSize<u32>,
}

impl VelloSurfaceRenderer {
    pub async fn new(
        window: &'static Window,
        present_mode: vello::wgpu::PresentMode,
    ) -> Result<Self> {
        let mut render_context = RenderContext::new();
        let size = window.inner_size();
        let surface = render_context
            .create_surface(window, size.width.max(1), size.height.max(1), present_mode)
            .await
            .map_err(|error| anyhow!("failed to create render surface: {error:?}"))?;
        let device = &render_context.devices[surface.dev_id].device;
        let renderer = Renderer::new(device, RendererOptions::default())
            .map_err(|error| anyhow!("failed to create vello renderer: {error:?}"))?;

        Ok(Self {
            render_context,
            surface,
            renderer,
            scene: Scene::new(),
            size,
        })
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        self.size = size;
        if size.width > 0 && size.height > 0 {
            self.render_context
                .resize_surface(&mut self.surface, size.width, size.height);
        }
    }

    pub fn render(&mut self, frame: &FrameView<'_>) -> Result<()> {
        if self.size.width == 0 || self.size.height == 0 {
            return Ok(());
        }

        let device_handle = &self.render_context.devices[self.surface.dev_id];
        render_to_scene(&mut self.scene, frame);
        self.renderer
            .render_to_texture(
                &device_handle.device,
                &device_handle.queue,
                &self.scene,
                &self.surface.target_view,
                &RenderParams {
                    base_color: vello::peniko::Color::BLACK,
                    width: self.size.width,
                    height: self.size.height,
                    antialiasing_method: aa_config(),
                },
            )
            .map_err(|error| anyhow!("failed to render vello scene: {error:?}"))?;

        let surface_texture = self
            .surface
            .surface
            .get_current_texture()
            .context("failed to acquire swapchain texture")?;
        let surface_view = surface_texture
            .texture
            .create_view(&vello::wgpu::TextureViewDescriptor::default());
        let mut encoder =
            device_handle
                .device
                .create_command_encoder(&vello::wgpu::CommandEncoderDescriptor {
                    label: Some("trackervis-surface"),
                });
        self.surface.blitter.copy(
            &device_handle.device,
            &mut encoder,
            &self.surface.target_view,
            &surface_view,
        );
        device_handle.queue.submit(Some(encoder.finish()));
        surface_texture.present();
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub struct VelloImageRenderer {
    render_context: RenderContext,
    device_id: usize,
    renderer: Renderer,
    scene: Scene,
    target_texture: vello::wgpu::Texture,
    target_view: vello::wgpu::TextureView,
    readback: ReadbackBuffer,
    pub size: PhysicalSize<u32>,
}

#[cfg(not(target_arch = "wasm32"))]
struct ReadbackBuffer {
    buffer: vello::wgpu::Buffer,
    row_bytes: usize,
    padded_bytes_per_row: u32,
}

#[cfg(not(target_arch = "wasm32"))]
impl VelloImageRenderer {
    pub async fn new(width: u32, height: u32) -> Result<Self> {
        let mut render_context = RenderContext::new();
        let dev_id = render_context
            .device(None)
            .await
            .ok_or_else(|| anyhow!("failed to obtain a wgpu device"))?;
        let device_handle = &render_context.devices[dev_id];
        let (target_texture, target_view) =
            create_offscreen_targets(&device_handle.device, width.max(1), height.max(1));
        let readback = create_readback_buffer(&device_handle.device, width.max(1), height.max(1));
        let renderer = Renderer::new(&device_handle.device, RendererOptions::default())
            .map_err(|error| anyhow!("failed to create vello renderer: {error:?}"))?;

        Ok(Self {
            render_context,
            device_id: dev_id,
            renderer,
            scene: Scene::new(),
            target_texture,
            target_view,
            readback,
            size: PhysicalSize::new(width.max(1), height.max(1)),
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if self.size.width == width && self.size.height == height {
            return;
        }
        self.size = PhysicalSize::new(width, height);
        let device_handle = &self.render_context.devices[self.device_id];
        let (target_texture, target_view) =
            create_offscreen_targets(&device_handle.device, width, height);
        let readback = create_readback_buffer(&device_handle.device, width, height);
        self.target_texture = target_texture;
        self.target_view = target_view;
        self.readback = readback;
    }

    pub fn render(&mut self, frame: &FrameView<'_>) -> Result<Vec<u8>> {
        let width = frame.width.max(1);
        let height = frame.height.max(1);
        if self.size.width != width || self.size.height != height {
            self.resize(width, height);
        }

        let device_handle = &self.render_context.devices[self.device_id];
        render_to_scene(&mut self.scene, frame);
        self.renderer
            .render_to_texture(
                &device_handle.device,
                &device_handle.queue,
                &self.scene,
                &self.target_view,
                &RenderParams {
                    base_color: vello::peniko::Color::BLACK,
                    width,
                    height,
                    antialiasing_method: aa_config(),
                },
            )
            .map_err(|error| anyhow!("failed to render vello scene: {error:?}"))?;

        readback_rgba(
            &device_handle.device,
            &device_handle.queue,
            &self.target_texture,
            &self.readback,
            width,
            height,
        )
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn create_readback_buffer(device: &vello::wgpu::Device, width: u32, height: u32) -> ReadbackBuffer {
    let row_bytes_u32 = width.checked_mul(4).expect("image width is too large");
    let row_bytes = row_bytes_u32 as usize;
    let padded_bytes_per_row =
        vello::wgpu::util::align_to(row_bytes_u32, vello::wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer_size = padded_bytes_per_row as u64 * height as u64;
    let buffer = device.create_buffer(&vello::wgpu::BufferDescriptor {
        label: Some("trackervis-offscreen-readback"),
        size: buffer_size,
        usage: vello::wgpu::BufferUsages::COPY_DST | vello::wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    ReadbackBuffer {
        buffer,
        row_bytes,
        padded_bytes_per_row,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn create_offscreen_targets(
    device: &vello::wgpu::Device,
    width: u32,
    height: u32,
) -> (vello::wgpu::Texture, vello::wgpu::TextureView) {
    let texture = device.create_texture(&vello::wgpu::TextureDescriptor {
        label: Some("trackervis-offscreen-target"),
        size: vello::wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: vello::wgpu::TextureDimension::D2,
        format: vello::wgpu::TextureFormat::Rgba8Unorm,
        usage: vello::wgpu::TextureUsages::STORAGE_BINDING
            | vello::wgpu::TextureUsages::TEXTURE_BINDING
            | vello::wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&vello::wgpu::TextureViewDescriptor::default());
    (texture, view)
}

#[cfg(not(target_arch = "wasm32"))]
fn readback_rgba(
    device: &vello::wgpu::Device,
    queue: &vello::wgpu::Queue,
    texture: &vello::wgpu::Texture,
    readback: &ReadbackBuffer,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    let mut encoder = device.create_command_encoder(&vello::wgpu::CommandEncoderDescriptor {
        label: Some("trackervis-offscreen-readback"),
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        vello::wgpu::TexelCopyBufferInfo {
            buffer: &readback.buffer,
            layout: vello::wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(readback.padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        vello::wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let (tx, rx) = mpsc::channel();
    readback
        .buffer
        .slice(..)
        .map_async(vello::wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    device
        .poll(vello::wgpu::PollType::wait_indefinitely())
        .map_err(|error| anyhow!("failed to wait for GPU readback: {error:?}"))?;

    rx.recv()
        .map_err(|_| anyhow!("failed to receive GPU readback completion"))?
        .map_err(|error| anyhow!("failed to map readback buffer: {error:?}"))?;

    let mapped = readback.buffer.slice(..).get_mapped_range();
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    for (row_index, src_row) in mapped
        .chunks_exact(readback.padded_bytes_per_row as usize)
        .enumerate()
        .take(height as usize)
    {
        let dst_offset = row_index * readback.row_bytes;
        pixels[dst_offset..dst_offset + readback.row_bytes]
            .copy_from_slice(&src_row[..readback.row_bytes]);
    }
    drop(mapped);
    readback.buffer.unmap();
    Ok(pixels)
}

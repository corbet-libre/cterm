//! Metal-based terminal renderer
//!
//! Renders the terminal content using Metal for GPU acceleration.

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::NSView;
use objc2_foundation::{NSRect, NSSize};
use objc2_metal::{
    MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLDevice, MTLDrawable,
    MTLPixelFormat, MTLRenderPassDescriptor,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};

use cterm_core::Terminal;
use cterm_ui::theme::Theme;

/// Metal renderer for terminal display
pub struct MetalRenderer {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    command_queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    layer: Retained<CAMetalLayer>,
    theme: Theme,
}

impl MetalRenderer {
    /// Create a new Metal renderer
    pub fn new(view: &NSView, theme: &Theme) -> Result<Self, String> {
        // Get the default Metal device
        let device = unsafe {
            objc2_metal::MTLCreateSystemDefaultDevice()
                .ok_or_else(|| "Failed to create Metal device".to_string())?
        };

        // Create command queue
        let command_queue = device
            .newCommandQueue()
            .ok_or_else(|| "Failed to create command queue".to_string())?;

        // Create Metal layer
        let layer = unsafe { CAMetalLayer::new() };
        layer.setDevice(Some(&device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setFramebufferOnly(true);

        // Get view frame for initial size
        let frame: NSRect = unsafe { msg_send![view, frame] };
        layer.setDrawableSize(frame.size);

        // Set layer on view
        unsafe {
            let _: () = msg_send![view, setLayer: &*layer];
            let _: () = msg_send![view, setWantsLayer: true];
        }

        Ok(Self {
            device,
            command_queue,
            layer,
            theme: theme.clone(),
        })
    }

    /// Render the terminal content
    pub fn render(&self, _terminal: &Terminal) {
        // Get drawable
        let Some(drawable) = self.layer.nextDrawable() else {
            log::warn!("No drawable available");
            return;
        };

        // Get drawable texture
        let texture = drawable.texture();

        // Create render pass descriptor
        let pass_descriptor = MTLRenderPassDescriptor::new();

        // Configure color attachment
        let color_attachment = unsafe {
            pass_descriptor
                .colorAttachments()
                .objectAtIndexedSubscript(0)
        };
        color_attachment.setTexture(Some(&texture));
        color_attachment.setLoadAction(objc2_metal::MTLLoadAction::Clear);
        color_attachment.setStoreAction(objc2_metal::MTLStoreAction::Store);

        // Set clear color from theme background
        let bg = &self.theme.colors.background;
        let clear_color = MTLClearColor {
            red: bg.r as f64 / 255.0,
            green: bg.g as f64 / 255.0,
            blue: bg.b as f64 / 255.0,
            alpha: 1.0,
        };
        color_attachment.setClearColor(clear_color);

        // Create command buffer
        let Some(command_buffer) = self.command_queue.commandBuffer() else {
            log::error!("Failed to create command buffer");
            return;
        };

        // Create render encoder
        let Some(encoder) = command_buffer.renderCommandEncoderWithDescriptor(&pass_descriptor)
        else {
            log::error!("Failed to create render encoder");
            return;
        };

        // For now, just clear the background
        // TODO: Implement actual glyph rendering with texture atlas
        //
        // A full implementation would:
        // 1. Iterate over terminal cells
        // 2. For each cell, draw a background quad with the cell's bg color
        // 3. Draw the glyph from the texture atlas with the cell's fg color
        // 4. Handle cursor rendering
        // 5. Handle selection highlighting

        // End encoding
        encoder.endEncoding();

        // Present and commit
        // CAMetalDrawable extends MTLDrawable, use the as_super() pattern
        unsafe {
            let drawable_ptr =
                drawable.as_ref() as *const _ as *const ProtocolObject<dyn MTLDrawable>;
            command_buffer.presentDrawable(&*drawable_ptr);
        }
        command_buffer.commit();
    }

    /// Update drawable size when view resizes
    pub fn resize(&self, size: NSSize) {
        self.layer.setDrawableSize(size);
    }

    /// Update theme colors
    pub fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }
}

impl Drop for MetalRenderer {
    fn drop(&mut self) {
        // Metal resources are automatically released via Retained
    }
}

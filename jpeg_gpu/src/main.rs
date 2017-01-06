extern crate gleam;
extern crate glutin;
extern crate image;
extern crate webrender;
extern crate webrender_traits;

use gleam::gl;
use image::GenericImage;
use std::fs::File;
use std::io::Read;
use std::env;
use webrender::{ExternalImageHandler, ExternalImage, ExternalImageSource};
use webrender_traits::{ColorF, Epoch, ExternalImageId, ImageData, ImageFormat, ImageRendering};
use webrender_traits::{ClipRegion, DeviceUintSize, DisplayListBuilder, LayoutPoint, LayoutRect, LayoutSize, LayoutTransform, PipelineId, RendererKind};

fn load_file(name: &str) -> Vec<u8> {
    let mut file = File::open(name).unwrap();
    let mut buffer = vec![];
    file.read_to_end(&mut buffer).unwrap();
    buffer
}

struct Handler;

impl ExternalImageHandler for Handler {
    fn lock(&mut self, key: ExternalImageId) -> ExternalImage {
        //println!("in lock, key = {:?}", key);
        ExternalImage {u0: 0.0, v0: 0.0, u1: 1.0, v1: 1.0, source: ExternalImageSource::NativeTexture(0) }
    }
    fn unlock(&mut self, key: ExternalImageId) {
        //println!("in unlock, key = {:?}", key);
    }
    fn release(&mut self, key: ExternalImageId) {
        //println!("in release, key = {:?}", key);
    }
}

struct Notifier {
    window_proxy: glutin::WindowProxy,
}

impl Notifier {
    fn new(window_proxy: glutin::WindowProxy) -> Notifier {
        Notifier {
            window_proxy: window_proxy,
        }
    }
}

impl webrender_traits::RenderNotifier for Notifier {
    fn new_frame_ready(&mut self) {
        self.window_proxy.wakeup_event_loop();
    }

    fn new_scroll_frame_ready(&mut self, _composite_needed: bool) {
        self.window_proxy.wakeup_event_loop();
    }

    fn pipeline_size_changed(&mut self,
                             _: PipelineId,
                             _: Option<LayoutSize>) {
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() > 1 {
        let name = &args[1];
        let img = image::load_from_memory_with_format(&load_file(name), image::ImageFormat::JPEG);
        match img {
            Ok(data) => {
                let (width, height) = data.dimensions();
                println!("Image size {}x{}", width, height);

                let window = glutin::WindowBuilder::new()
                            .with_title("WebRender JPEG GPU")
                            .with_dimensions(width, height)
                            .with_gl(glutin::GlRequest::Specific(glutin::Api::OpenGl, (3, 2)))
                            .build()
                            .unwrap();

                unsafe {
                    window.make_current().ok();
                    gl::load_with(|symbol| window.get_proc_address(symbol) as *const _);
                    gl::clear_color(0.3, 0.0, 0.0, 1.0);
                }

                println!("OpenGL version {}", gl::get_string(gl::VERSION));

                {
                    let (width, height) = window.get_inner_size().unwrap();

                    println!("Width {} height {}", width, height);
                }

                let opts = webrender::RendererOptions {
                    device_pixel_ratio: 1.0,
                    resource_override_path: None,
                    enable_aa: false,
                    enable_profiler: true,
                    enable_recording: false,
                    enable_scrollbars: false,
                    debug: true,
                    precache_shaders: true,
                    renderer_kind: RendererKind::Native,
                    enable_subpixel_aa: false,
                    clear_framebuffer: true,
                    clear_color: ColorF::new(1.0, 1.0, 1.0, 1.0),
                    render_target_debug: false,
                };

                let (mut renderer, sender) = webrender::renderer::Renderer::new(opts);
                let api = sender.create_api();

                let notifier = Box::new(Notifier::new(window.create_window_proxy()));
                renderer.set_render_notifier(notifier);

                let epoch = Epoch(0);
                let root_background_color = ColorF::new(0.3, 0.0, 0.0, 1.0);

                let pipeline_id = PipelineId(0, 0);
                let mut builder = DisplayListBuilder::new(pipeline_id);

                    let bounds = LayoutRect::new(LayoutPoint::new(0.0, 0.0), LayoutSize::new(width as f32, height as f32));
                    let clip_region = builder.new_clip_region(&bounds, vec![], None);

                    builder.push_stacking_context(webrender_traits::ScrollPolicy::Scrollable,
                                                  bounds,
                                                  clip_region,
                                                  0,
                                                  &LayoutTransform::identity(),
                                                  &LayoutTransform::identity(),
                                                  webrender_traits::MixBlendMode::Normal,
                                                  Vec::new());

                renderer.set_external_image_handler(Box::new(Handler));

                let bounds = LayoutRect::new(LayoutPoint::new(0.0, 0.0), LayoutSize::new(width as f32, height as f32));
                let clip = ClipRegion::simple(&bounds);
                let size = LayoutSize::new(width as f32, height as f32);
                let spacing = LayoutSize::new(0.0, 0.0);

                builder.push_image(bounds, clip, size, spacing, ImageRendering::Auto,
                    api.add_image(width, height, None, ImageFormat::RGBA8, ImageData::External(ExternalImageId(0))));

                builder.pop_stacking_context();

                api.set_root_display_list(
                    Some(root_background_color),
                    epoch,
                    LayoutSize::new(width as f32, height as f32),
                    builder);
                api.set_root_pipeline(pipeline_id);

                let mut done = false;

                while !done {
                    gl::clear(gl::COLOR_BUFFER_BIT);
                    renderer.update();

                    renderer.render(DeviceUintSize::new(width, height));

                    window.swap_buffers().ok();

                    for event in window.poll_events() {
                        match event {
                            glutin::Event::Closed => done = true,
                            glutin::Event::KeyboardInput(_element_state, scan_code, _virtual_key_code) => {
                                if scan_code == 9 {
                                    done = true;
                                }
                            }
                            _ => ()
                        }
                    }
                }
            }
            Err(err) => {
                println!("Error loading image {}: {}", name, err)
            }
        }
    }
}
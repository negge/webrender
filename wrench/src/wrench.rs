/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use app_units::Au;
#[cfg(windows)]
use dwrote;
#[cfg(target_os = "linux")]
use font_loader::system_fonts;

use gleam::gl;
use glutin;
use glutin::{WindowProxy, VirtualKeyCode};
use image;
use image::GenericImage;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use webrender;
use webrender_traits::*;
use yaml_rust::Yaml;
use yaml_frame_writer::YamlFrameWriter;
use json_frame_writer::JsonFrameWriter;
use time;
use crossbeam::sync::chase_lev;
use WindowWrapper;

use {WHITE_COLOR, BLACK_COLOR};

pub enum SaveType {
    Yaml,
    Json,
}

struct Notifier {
    window_proxy: Option<WindowProxy>,
    frames_notified: u32,
    timing_receiver: chase_lev::Stealer<time::SteadyTime>,
    verbose: bool,
}

impl Notifier {
    fn new(window_proxy: Option<WindowProxy>, timing_receiver: chase_lev::Stealer<time::SteadyTime>, verbose: bool) -> Notifier {
        Notifier {
            window_proxy: window_proxy,
            frames_notified: 0,
            timing_receiver: timing_receiver,
            verbose: verbose,
        }
    }
}

impl RenderNotifier for Notifier {
    fn new_frame_ready(&mut self) {
        match self.timing_receiver.steal() {
            chase_lev::Steal::Data(last_timing) => {
                self.frames_notified += 1;
                if self.verbose && self.frames_notified == 600 {
                    let elapsed = time::SteadyTime::now() - last_timing;
                    println!("frame latency (consider queue depth when looking at this number): {:3.6} ms",
                             elapsed.num_microseconds().unwrap() as f64 / 1000.);
                    self.frames_notified = 0;
                }
            }
            _ => {
                println!("Notified of frame, but no frame was ready?");
            }
        }
        if let Some(ref window_proxy) = self.window_proxy {
            window_proxy.wakeup_event_loop();
        }
    }

    fn new_scroll_frame_ready(&mut self, _composite_needed: bool) {
        if let Some(ref window_proxy) = self.window_proxy {
            window_proxy.wakeup_event_loop();
        }
    }

    fn pipeline_size_changed(&mut self,
                             _: PipelineId,
                             _: Option<LayoutSize>) {
    }
}

#[cfg(target_os = "windows")]
pub fn layout_simple_ascii(face: NativeFontHandle, text: &str, size: Au) -> (Vec<u16>, Vec<f32>) {
    let system_fc = dwrote::FontCollection::system();
    let font = system_fc.get_font_from_descriptor(&(face as dwrote::FontDescriptor)).unwrap();
    let face = font.create_font_face();

    let chars: Vec<u32> = text.chars().map(|c| c as u32).collect();
    let indices = face.get_glyph_indices(&chars);
    let glyph_metrics = face.get_design_glyph_metrics(&indices, false);

    let device_pixel_ratio: f32 = 1.0;
    let em_size = size.to_f32_px() / 16.;
    let design_units_per_pixel = face.metrics().designUnitsPerEm as f32 / 16. as f32;
    let scaled_design_units_to_pixels = (em_size * device_pixel_ratio) / design_units_per_pixel;

    let advances = glyph_metrics.iter().map(|m| m.advanceWidth as f32 * scaled_design_units_to_pixels).collect();

    (indices, advances)
}

#[cfg(not(target_os = "windows"))]
pub fn layout_simple_ascii(face: NativeFontHandle, text: &str, size: Au) -> (Vec<u16>, Vec<f32>) {
    panic!("Can't layout simple ascii on this platform");
}

pub trait WrenchThing {
    fn next_frame(&mut self);
    fn prev_frame(&mut self);
    fn do_frame(&mut self, &mut Wrench) -> u32;
    fn queue_frames(&self) -> u32 { 0 }
}

pub struct Wrench {
    window_size: DeviceUintSize,
    device_pixel_ratio: f32,

    pub renderer: webrender::renderer::Renderer,
    pub api: RenderApi,
    pub root_pipeline_id: PipelineId,

    window_title_to_set: Option<String>,

    sender: RenderApiSender,
    image_map: HashMap<PathBuf, (ImageKey, LayoutSize)>,

    // internal housekeeping
    next_scroll_layer_id: usize,

    gl_renderer: String,
    gl_version: String,

    pub rebuild_display_lists: bool,
    pub verbose: bool,

    pub frame_start_sender: chase_lev::Worker<time::SteadyTime>,
}

impl Wrench {
    pub fn new(window: &mut WindowWrapper,
               shader_override_path: Option<PathBuf>,
               dp_ratio: f32,
               save_type: Option<SaveType>,
               size: DeviceUintSize,
               do_rebuild: bool,
               subpixel_aa: bool,
               debug: bool,
               verbose: bool)
           -> Wrench
    {
        println!("Shader override path: {:?}", shader_override_path);

        if let Some(ref save_type) = save_type {
            let recorder = match save_type {
                &SaveType::Yaml => Box::new(YamlFrameWriter::new(&PathBuf::from("yaml_frames")))
                    as Box<webrender::ApiRecordingReceiver>,
                &SaveType::Json => Box::new(JsonFrameWriter::new(&PathBuf::from("json_frames")))
                    as Box<webrender::ApiRecordingReceiver>,
            };
            webrender::set_recording_detour(Some(recorder));
        }

        let opts = webrender::RendererOptions {
            device_pixel_ratio: dp_ratio,
            resource_override_path: shader_override_path,
            enable_aa: false,
            enable_profiler: false,
            enable_recording: save_type.is_some(),
            enable_scrollbars: false,
            enable_subpixel_aa: subpixel_aa,
            debug: debug,
            precache_shaders: false,
            renderer_kind: RendererKind::Native,
            clear_framebuffer: true,
            clear_color: ColorF::new(1.0, 1.0, 1.0, 1.0),
            render_target_debug: false,
        };

        let (renderer, sender) = webrender::renderer::Renderer::new(opts);
        let api = sender.create_api();

        let (timing_sender, timing_receiver) = chase_lev::deque();
        let notifier = Box::new(Notifier::new(window.create_window_proxy(), timing_receiver, verbose));
        renderer.set_render_notifier(notifier);

        let gl_version = gl::get_string(gl::VERSION);
        let gl_renderer = gl::get_string(gl::RENDERER);

        let mut wrench = Wrench {
            window_size: size,

            renderer: renderer,
            sender: sender,
            api: api,
            window_title_to_set: None,

            rebuild_display_lists: do_rebuild,
            verbose: verbose,
            device_pixel_ratio: dp_ratio,

            image_map: HashMap::new(),

            root_pipeline_id: PipelineId(0, 0),
            next_scroll_layer_id: 0,

            gl_renderer: gl_renderer,
            gl_version: gl_version,
            frame_start_sender: timing_sender,
        };

        wrench.set_title("start");
        // there's a "frame 0" that webrender itself renders; push this to
        // not confuse our notifier
        wrench.frame_start_sender.push(time::SteadyTime::now());
        wrench.api.set_root_pipeline(wrench.root_pipeline_id);

        wrench
    }

    pub fn set_title(&mut self, extra: &str) {
        self.window_title_to_set = Some(format!("Wrench: {} ({}x) - {} - {}", extra,
            self.device_pixel_ratio, self.gl_renderer, self.gl_version));
    }

    pub fn take_title(&mut self) -> Option<String> {
        self.window_title_to_set.take()
    }

    pub fn should_rebuild_display_lists(&self) -> bool {
        self.rebuild_display_lists
    }

    pub fn window_size_f32(&self) -> LayoutSize {
        return LayoutSize::new(self.window_size.width as f32,
                               self.window_size.height as f32)
    }

    pub fn next_scroll_layer_id(&mut self) -> ScrollLayerId {
        let scroll_layer_id = ServoScrollRootId(self.next_scroll_layer_id);
        self.next_scroll_layer_id += 1;
        ScrollLayerId::new(self.root_pipeline_id, 0, scroll_layer_id)
    }

    #[cfg(target_os = "windows")]
    pub fn font_key_from_native_handle(&mut self, descriptor: &NativeFontHandle) -> FontKey {
        self.api.add_native_font(descriptor.clone())
    }

    #[cfg(target_os = "windows")]
    pub fn font_key_from_name(&mut self, font_name: &str) -> (FontKey, Option<NativeFontHandle>) {
        let system_fc = dwrote::FontCollection::system();
        let family = system_fc.get_font_family_by_name(font_name).unwrap();
        let font = family.get_first_matching_font(dwrote::FontWeight::Regular,
                                                  dwrote::FontStretch::Normal,
                                                  dwrote::FontStyle::Normal);
        let descriptor = font.to_descriptor();
        let key = self.api.add_native_font(descriptor.clone());
        (key, Some(descriptor))
    }

    #[cfg(target_os = "windows")]
    pub fn font_key_from_yaml_table(&mut self, item: &Yaml) -> (FontKey, Option<NativeFontHandle>) {
        assert!(!item["family"].is_badvalue());
        let family = item["family"].as_str().unwrap();
        let weight = dwrote::FontWeight::from_u32(item["weight"].as_i64().unwrap_or(400) as u32);
        let style = dwrote::FontStyle::from_u32(item["style"].as_i64().unwrap_or(0) as u32);
        let stretch = dwrote::FontStretch::from_u32(item["stretch"].as_i64().unwrap_or(5) as u32);

        let desc = dwrote::FontDescriptor {
            family_name: family.to_owned(),
            weight: weight,
            style: style,
            stretch: stretch,
        };
        (self.font_key_from_native_handle(&desc), Some(desc))
    }

    #[cfg(target_os = "linux")]
    pub fn font_key_from_yaml_table(&mut self, item: &Yaml) -> (FontKey, Option<NativeFontHandle>) {
        let family = item["family"].as_str().unwrap();
        let property = system_fonts::FontPropertyBuilder::new().family(family).build();
        let (font, _) = system_fonts::get(&property).unwrap();
        self.font_key_from_bytes(font)
    }


    #[cfg(not(target_os = "windows"))]
    pub fn font_key_from_native_handle(&mut self, descriptor: &NativeFontHandle) -> FontKey {
        panic!("Can't font_key_from_native_handle on this platform");
    }

    #[cfg(not(target_os = "windows"))]
    pub fn font_key_from_name(&mut self, font_name: &str) -> (FontKey, Option<NativeFontHandle>) {
        panic!("Can't font_key_from_name on this platform");
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    pub fn font_key_from_yaml_table(&mut self, item: &Yaml) -> (FontKey, Option<NativeFontHandle>) {
        panic!("Can't font_key_from_yaml_table on this platform");
    }

    pub fn font_key_from_bytes(&mut self, bytes: Vec<u8>) -> (FontKey, Option<NativeFontHandle>) {
        let key = self.api.add_raw_font(bytes);
        (key, None)
    }

    pub fn add_or_get_image(&mut self, file: &Path) -> (ImageKey, LayoutSize) {
        let key = file.to_owned();
        if let Some(k) = self.image_map.get(&key) {
            return *k
        }

        let image = image::open(file).unwrap();
        let image_dims = image.dimensions();
        let image_key = self.api.add_image(image_dims.0, image_dims.1,
                                           None, // stride
                                           match image {
                                               image::ImageLuma8(_) => ImageFormat::A8,
                                               image::ImageRgb8(_) => ImageFormat::RGB8,
                                               image::ImageRgba8(_) => ImageFormat::RGBA8,
                                               _ => panic!("We don't support whatever your crazy image type is, come on"),
                                           },
                                           ImageData::Raw(Arc::new(image.raw_pixels())));

        let val = (image_key, LayoutSize::new(image_dims.0 as f32, image_dims.1 as f32));
        self.image_map.insert(key, val);
        val
    }

    pub fn update(&mut self, dim: DeviceUintSize) {
        if dim != self.window_size {
            gl::viewport(0, 0, dim.width as i32, dim.height as i32);
            self.window_size = dim;
        }
    }

    pub fn begin_frame(&mut self) {
        self.frame_start_sender.push(time::SteadyTime::now());
    }

    pub fn send_lists(&mut self, frame_number: u32, display_list: DisplayListBuilder) {
        self.begin_frame();

        let root_background_color = Some(ColorF::new(1.0, 1.0, 1.0, 1.0));
        self.api.set_root_display_list(root_background_color,
                                       Epoch(frame_number),
                                       self.window_size_f32(),
                                       display_list);
    }

    pub fn render(&mut self) {
        self.renderer.update();
        self.renderer.render(self.window_size);
    }

    pub fn refresh(&mut self) {
        self.begin_frame();
        self.api.generate_frame();
    }

    pub fn show_onscreen_help(&mut self) {
        let help_lines = [
            "Esc, Q - Quit",
            "H - Toggle help",
            "R - Toggle recreating display items each frame",
            "P - Toggle profiler"
        ];

        let color_and_offset = [ (*BLACK_COLOR, 2.0), (*WHITE_COLOR, 0.0) ];
        let dr = self.renderer.debug_renderer();

        for ref co in color_and_offset.iter() {
            let x = self.device_pixel_ratio * (15.0 + co.1);
            let mut y = self.device_pixel_ratio * (15.0 + co.1 + dr.line_height());
            for ref line in help_lines.iter() {
                dr.add_text(x, y, line, &co.0);
                y += self.device_pixel_ratio * dr.line_height();
            }
        }
    }
}

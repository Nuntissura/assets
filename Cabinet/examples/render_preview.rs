//! Software capture of actual egui tessellation. No window, process or GPU is launched.
use cabinet::{egui_view::CabinetView, *};
use egui::{
    epaint::{ImageData, Primitive},
    Context as EguiContext, TextureId,
};
use std::{collections::HashMap, io::Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output_path = std::env::args().nth(1).ok_or("pass an output .ppm path")?;
    let mode = std::env::args().nth(2).unwrap_or_default();
    let mut folders = vec![
        Entry::new("notes", None, "Notes", EntryKind::Folder),
        Entry::new(
            "projects",
            Some("notes".into()),
            "Projects",
            EntryKind::Folder,
        ),
        Entry::new(
            "cabinet",
            Some("projects".into()),
            "Cabinet",
            EntryKind::Folder,
        ),
        Entry::new(
            "design",
            Some("cabinet".into()),
            "Design.md",
            EntryKind::Leaf,
        ),
        Entry::new(
            "research",
            Some("cabinet".into()),
            "Research.md",
            EntryKind::Leaf,
        ),
        Entry::new(
            "daily",
            Some("notes".into()),
            "Daily notes",
            EntryKind::Folder,
        ),
        Entry::new(
            "today",
            Some("daily".into()),
            "2026-10-11.md",
            EntryKind::Leaf,
        ),
        Entry::new("assets", None, "Assets", EntryKind::Folder),
        Entry::new(
            "handpick",
            Some("assets".into()),
            "Handpick",
            EntryKind::Folder,
        ),
        Entry::new("archive", None, "Archive", EntryKind::Folder),
    ];
    folders[0].color = Some([112, 152, 238]);
    folders[2].color = Some([75, 190, 153]);
    folders[7].color = Some([230, 172, 82]);
    folders[3].tags = vec![
        Tag {
            id: "design".into(),
            label: "Design".into(),
            color: [75, 190, 153],
        },
        Tag {
            id: "rust".into(),
            label: "Rust".into(),
            color: [225, 145, 73],
        },
    ];
    let mut cabinet = Cabinet::new(Snapshot {
        context: Context {
            scope: "preview".into(),
            generation: 1,
            revision: 1,
        },
        entries: folders,
        hierarchy_complete: true,
    })?;
    cabinet.reveal("design")?;
    cabinet.expand("daily", true)?;
    cabinet.expand("assets", true)?;
    let context = EguiContext::default();
    context.set_visuals(egui::Visuals::dark());
    let mut view = CabinetView::new("cabinet-preview");
    view.enable_scheduler(peak::SchedulerSettings::default())
        .map_err(|error| format!("Preview scheduler: {error:?}"))?;
    let width = 900usize;
    let height = 600usize;
    let mut textures: HashMap<TextureId, egui::ColorImage> = HashMap::new();
    let mut pixels = vec![[24u8, 24, 27]; width * height];
    let mut layout: Option<cabinet::egui_view::ViewResponse> = None;
    let frame_count = if mode == "menu" || mode == "color" {
        12
    } else {
        8
    };
    for frame in 0..frame_count {
        let mut events = Vec::new();
        if let Some(previous) = &layout {
            let position = if mode == "peak" {
                previous.rows["design"].center()
            } else {
                previous.controls["options:design"].center()
            };
            if mode == "peak" {
                events.push(egui::Event::PointerMoved(position));
            } else if mode == "menu" || mode == "color" {
                if frame == 3 || frame == 4 {
                    events.push(egui::Event::PointerMoved(position));
                    events.push(egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: frame == 3,
                        modifiers: Default::default(),
                    });
                }
                if mode == "color" && (frame == 7 || frame == 8) {
                    if let Some(rect) = previous.controls.get("menu-color:design") {
                        let position = rect.center();
                        events.push(egui::Event::PointerMoved(position));
                        events.push(egui::Event::PointerButton {
                            pos: position,
                            button: egui::PointerButton::Primary,
                            pressed: frame == 7,
                            modifiers: Default::default(),
                        });
                    }
                }
            }
        }
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width as f32, height as f32),
            )),
            time: Some(frame as f64 * if mode == "peak" { 0.4 } else { 0.02 }),
            events,
            ..Default::default()
        };
        let output = context.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("Cabinet");
                ui.label("Reusable tree · host-owned data");
                ui.separator();
                let result = view.show(ui, &mut cabinet);
                layout = Some(result);
            });
        });
        // The software fixture completes tiny text jobs between UI frames; real hosts use workers.
        if let Some(response) = &layout {
            for job in &response.preview_jobs {
                let content=peak::decode_text(b"# Cabinet design\n\n- Reusable hierarchy mechanics\n- Host-owned durable operations\n- Peak: names, tags and timed previews\n- Scoped persistent folder settings\n",&job.settings,false);
                view.complete_scheduled(job.token, content);
            }
        }
        for (id, delta) in &output.textures_delta.set {
            let ImageData::Color(image) = &delta.image;
            if let Some([x, y]) = delta.pos {
                let texture = textures.get_mut(id).ok_or("partial unknown texture")?;
                for row in 0..image.size[1] {
                    let dest = (row + y) * texture.size[0] + x;
                    texture.pixels[dest..dest + image.size[0]].copy_from_slice(
                        &image.pixels[row * image.size[0]..(row + 1) * image.size[0]],
                    );
                }
            } else {
                textures.insert(*id, (**image).clone());
            }
        }
        if frame + 1 == frame_count {
            for clipped in context.tessellate(output.shapes, output.pixels_per_point) {
                let Primitive::Mesh(mesh) = clipped.primitive else {
                    continue;
                };
                let Some(texture) = textures.get(&mesh.texture_id) else {
                    continue;
                };
                for triangle in mesh.indices.chunks_exact(3) {
                    let a = mesh.vertices[triangle[0] as usize];
                    let b = mesh.vertices[triangle[1] as usize];
                    let c = mesh.vertices[triangle[2] as usize];
                    let min_x = a
                        .pos
                        .x
                        .min(b.pos.x)
                        .min(c.pos.x)
                        .max(clipped.clip_rect.min.x)
                        .max(0.0)
                        .floor() as usize;
                    let min_y = a
                        .pos
                        .y
                        .min(b.pos.y)
                        .min(c.pos.y)
                        .max(clipped.clip_rect.min.y)
                        .max(0.0)
                        .floor() as usize;
                    let max_x = a
                        .pos
                        .x
                        .max(b.pos.x)
                        .max(c.pos.x)
                        .min(clipped.clip_rect.max.x)
                        .min(width as f32)
                        .ceil() as usize;
                    let max_y = a
                        .pos
                        .y
                        .max(b.pos.y)
                        .max(c.pos.y)
                        .min(clipped.clip_rect.max.y)
                        .min(height as f32)
                        .ceil() as usize;
                    let edge = |p: egui::Pos2, q: egui::Pos2, r: egui::Pos2| {
                        (r.x - p.x) * (q.y - p.y) - (r.y - p.y) * (q.x - p.x)
                    };
                    let area = edge(a.pos, b.pos, c.pos);
                    if area.abs() < 1e-6 {
                        continue;
                    }
                    for y in min_y..max_y {
                        for x in min_x..max_x {
                            let p = egui::pos2(x as f32 + 0.5, y as f32 + 0.5);
                            let wa = edge(b.pos, c.pos, p) / area;
                            let wb = edge(c.pos, a.pos, p) / area;
                            let wc = 1.0 - wa - wb;
                            if wa < 0.0 || wb < 0.0 || wc < 0.0 {
                                continue;
                            }
                            let u = wa * a.uv.x + wb * b.uv.x + wc * c.uv.x;
                            let v = wa * a.uv.y + wb * b.uv.y + wc * c.uv.y;
                            let tx = (u * texture.size[0] as f32)
                                .floor()
                                .max(0.0)
                                .min((texture.size[0] - 1) as f32)
                                as usize;
                            let ty = (v * texture.size[1] as f32)
                                .floor()
                                .max(0.0)
                                .min((texture.size[1] - 1) as f32)
                                as usize;
                            let tex = texture.pixels[ty * texture.size[0] + tx].to_array();
                            let ca = a.color.to_array();
                            let cb = b.color.to_array();
                            let cc = c.color.to_array();
                            let color: [f32; 4] = std::array::from_fn(|i| {
                                (wa * ca[i] as f32 + wb * cb[i] as f32 + wc * cc[i] as f32)
                                    * tex[i] as f32
                                    / 255.0
                            });
                            let alpha = color[3] / 255.0;
                            for channel in 0..3 {
                                pixels[y * width + x][channel] = (color[channel]
                                    + pixels[y * width + x][channel] as f32 * (1.0 - alpha))
                                    .clamp(0.0, 255.0)
                                    as u8;
                            }
                        }
                    }
                }
            }
        }
    }
    let final_layout = layout.as_ref().ok_or("capture produced no UI response")?;
    if mode == "menu" && !final_layout.controls.contains_key("menu-download:design") {
        return Err("menu capture failed: expected design Download menu control".into());
    }
    if mode == "color"
        && !(final_layout.controls.contains_key("color-hex")
            && final_layout.controls.contains_key("color-wheel"))
    {
        return Err("color capture failed: expected hex field and color wheel".into());
    }
    let mut file = std::fs::File::create(&output_path)?;
    write!(file, "P6\n{width} {height}\n255\n")?;
    for pixel in pixels {
        file.write_all(&pixel)?;
    }
    println!("Rendered Cabinet preview: {output_path}");
    Ok(())
}

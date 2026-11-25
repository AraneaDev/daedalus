use super::{QuadInstance, Rect, Rgba, Theme, push_rect_border};
use daedalus_proto::rpc::ImagePreview;

pub(super) fn draw(
    quads: &mut Vec<QuadInstance>,
    panel: Rect,
    image: &ImagePreview,
    theme: &Theme,
    s: f32,
    border_w: f32,
) {
    quads.push(QuadInstance::new(panel, theme.bg_sidebar));
    let inset = 4. * s;
    let header = 24. * s;
    let pixel = ((panel.width - 2. * inset) / image.width as f32)
        .min((panel.height - header - inset) / image.height as f32);
    if pixel > 0. {
        let left = panel.x + (panel.width - pixel * image.width as f32) / 2.;
        for (y, row) in image
            .rgba
            .chunks_exact(image.width as usize * 4)
            .enumerate()
        {
            let mut x = 0;
            while x < image.width as usize {
                let start = x;
                let color = &row[x * 4..x * 4 + 4];
                x += 1;
                while x < image.width as usize && &row[x * 4..x * 4 + 4] == color {
                    x += 1;
                }
                quads.push(QuadInstance::from_xywh(
                    left + start as f32 * pixel,
                    panel.y + header + y as f32 * pixel,
                    (x - start) as f32 * pixel,
                    pixel,
                    Rgba::from_rgba_u8(color[0], color[1], color[2], color[3]),
                ));
            }
        }
    }
    push_rect_border(quads, panel, border_w, theme.border_unfocused);
}

use daedalus_proto::PaneRole;
use daedalus_ui::layout::{Divider, WorkspaceLayout};

use super::DaedalusApp;

#[derive(Clone, Copy)]
pub(super) struct DividerDrag {
    pub(super) divider: Divider,
    origin: f32,
    extent: f32,
    offset: f32,
}

pub(super) fn divider_at(
    layout: &WorkspaceLayout,
    x: f32,
    y: f32,
    scale: f32,
) -> Option<DividerDrag> {
    let agent = layout.pane(PaneRole::Agent)?.outer;
    let service = layout.pane(PaneRole::Service)?.outer;
    let runner = layout.pane(PaneRole::Runner)?.outer;
    let reach = 4. * scale;
    let drag = |divider, origin, end, center, pointer| {
        Some(DividerDrag {
            divider,
            origin,
            extent: end - origin,
            offset: center - pointer,
        })
    };
    if layout.stacked {
        if x < agent.x || x >= agent.right() {
            return None;
        }
        for (divider, center) in [
            (Divider::UpperRow, service.y),
            (Divider::LowerRow, runner.y),
        ] {
            if (y - center).abs() <= reach {
                return drag(divider, agent.y, runner.bottom(), center, y);
            }
        }
    } else {
        let gutter = (service.x - agent.right()) / 2.;
        let center = agent.right() + gutter;
        if (x - center).abs() <= reach && y >= agent.y && y <= agent.bottom() {
            return drag(
                Divider::Column,
                agent.x - gutter,
                service.right() + gutter,
                center,
                x,
            );
        }
        let center = service.bottom() + gutter;
        if x >= service.x && x <= service.right() && (y - center).abs() <= reach {
            return drag(
                Divider::RightRow,
                service.y - gutter,
                runner.bottom() + gutter,
                center,
                y,
            );
        }
    }
    None
}

impl DaedalusApp {
    pub(super) fn drag_divider(&mut self, x: f32, y: f32) -> bool {
        let Some(drag) = self.pointer.divider_drag else {
            return false;
        };
        let coordinate = if drag.divider == Divider::Column {
            x
        } else {
            y
        };
        if drag.extent > 0. {
            self.pointer.splits.move_divider(
                drag.divider,
                (coordinate + drag.offset - drag.origin) / drag.extent,
            );
            self.schedule_resize_flush();
            self.request_redraw();
        }
        true
    }
}

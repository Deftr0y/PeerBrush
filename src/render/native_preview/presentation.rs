//! Render scalar animation without opening an edit or creating history.
use super::*;
use crate::engine::scene::presentation::Presentation;

pub(crate) fn render(
    base: Arc<SceneSnapshot>,
    session: SourceReadSession,
    presentation: &Presentation,
    rect: Option<[i32; 4]>,
    edge: u32,
    target: Option<&str>,
    mask: bool,
    check: &dyn Fn() -> Result<()>,
) -> Result<(Frame, Arc<Origin>)> {
    render_with(base, session, presentation, rect, edge, target, mask, check, false,
        |request, check| request.render(&mut || check()))
}

pub(crate) fn render_with<T: DisplayOutput>(
    base: Arc<SceneSnapshot>,
    session: SourceReadSession,
    presentation: &Presentation,
    rect: Option<[i32; 4]>,
    edge: u32,
    target: Option<&str>,
    mask: bool,
    check: &dyn Fn() -> Result<()>,
    canvas: bool,
    render: impl FnOnce(&Request, &dyn Fn() -> Result<()>) -> Result<T>,
) -> Result<(T, Arc<Origin>)> {
    check()?;
    session.check_scene(&base)?;
    let scene = Arc::new(presentation.scene(&base, check)?);
    let request = Request::capture(scene, session, rect, edge, target, mask)?;
    let request = if canvas { request.for_canvas(edge)? } else { request };
    let origin = Origin::presentation(&base, &request, presentation.signature())?;
    let frame = render(&request, check)?;
    if !origin.candidate_tag().same_allocation(&frame.display_tag()) {
        return Err("Animation pixels differ from their presentation request".into());
    }
    drop(request);
    check()?;
    Ok((frame, origin))
}

#[cfg(test)]
#[path = "presentation_tests.rs"]
mod tests;

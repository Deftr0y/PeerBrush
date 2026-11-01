#[path = "../../svg-origin-corpus.rs"]
mod corpus;
use std::io::{self, Write};
fn main() {
    let baseline = std::env::args().any(|argument| argument == "--baseline");
    let mut out = io::BufWriter::new(io::stdout().lock());
    for case in corpus::cases() {
        if baseline && case.name == "reused-identical-clip-sites" {
            continue;
        }

        let tree =
            resvg::usvg::Tree::from_str(&case.svg, &resvg::usvg::Options::default()).unwrap();
        let [sx, ky, kx, sy, tx, ty] = case.matrix;
        let transform = resvg::tiny_skia::Transform::from_row(sx, ky, kx, sy, tx, ty);
        let mut image = resvg::tiny_skia::Pixmap::new(case.frame[0], case.frame[1]).unwrap();
        resvg::render(&tree, transform, &mut image.as_mut());
        // Candidate emits the same selected rows as single rows and bands.
        for y in case
            .rows
            .iter()
            .cycle()
            .take(case.rows.len() * if baseline { 2 } else { 3 })
        {
            let y = *y;
            let start = y as usize * case.frame[0] as usize * 4;
            out.write_all(&image.data()[start..start + case.frame[0] as usize * 4])
                .unwrap();
        }
        eprintln!(
            "registry oracle: {} {:?} {:?}",
            case.name, case.frame, case.matrix
        );
    }
    out.flush().unwrap();
}

//! Synthetic renderer-only corpus. Shared by the unmodified registry oracle
//! and the row candidate; it imports no application, image decoder or staging.
pub struct Case {
    pub name: &'static str,
    pub frame: [u32; 2],
    pub matrix: [f32; 6],
    pub svg: String,
    pub rows: Vec<u32>,
}
pub fn cases() -> Vec<Case> {
    let mut result = Vec::new();
    let matrices = [
        [1., 0., 0., 1., 0., 0.],
        [0.9375, 0.125, -0.0625, 1.125, 7.25, 0.625],
        [-0.875, 0.0625, 0.125, 1.0625, 108.125, -2.25],
    ];
    let bodies = [
        ("rounded-fractional", "<rect x='5.25' y='3.125' width='83.375' height='49.625' rx='9.375' fill='#db317f' fill-opacity='.73'/><path d='M3.125 68.375 C39.25 -11.5 77.125 93.5 104.75 7.25' fill='none' stroke='#245bae' stroke-width='1.375' stroke-linecap='round'/>") ,
        ("gradient-dash", "<defs><linearGradient id='l' x1='3.25' y1='4.5' x2='101.25' y2='67.75' gradientUnits='userSpaceOnUse' spreadMethod='reflect'><stop offset='0' stop-color='#ff2311' stop-opacity='.31'/><stop offset='.43' stop-color='#2277dd' stop-opacity='.81'/><stop offset='1' stop-color='#11c34e' stop-opacity='.53'/></linearGradient><radialGradient id='r' cx='.43' cy='.62' r='.57' fx='.27' fy='.23'><stop stop-color='#bd281b'/><stop offset='.31' stop-color='#4375cc' stop-opacity='.29'/><stop offset='1' stop-color='#c82bc1' stop-opacity='.83'/></radialGradient></defs><rect x='1.125' y='2.375' width='104.25' height='66.75' fill='url(#l)'/><path d='M8.25 5.5 Q105.25 12.75 43.5 67.125 T9.75 16.5' fill='url(#r)' stroke='#193e71' stroke-width='3.625' stroke-linejoin='round' stroke-dasharray='4.25 1.375 2.5 3.125' stroke-dashoffset='1.375'/>") ,
        ("groups-clips", "<defs><clipPath id='outer'><rect x='7.125' y='4.25' width='96.375' height='67.625' rx='13.375'/></clipPath><clipPath id='inner' clip-path='url(#outer)'><g clip-path='url(#outer)' transform='translate(.375 -.625)'><path d='M1 1 H109 V75 H1 Z M37.25 17.375 V55.125 H76.625 V17.375 Z' clip-rule='evenodd'/></g></clipPath></defs><g opacity='.61' clip-path='url(#inner)' transform='translate(.625 .375)'><rect x='2.25' y='1.75' width='103.375' height='72.125' fill='#e25337'/><g opacity='.73' style='mix-blend-mode:multiply'><ellipse cx='65.375' cy='37.625' rx='38.125' ry='26.875' fill='#2375cb'/><path d='M4.625 5.375 L104.25 66.875 L17.125 72.625 Z' fill='#55bf79' fill-opacity='.47'/></g></g>") ,
    ];
    for (name, body) in bodies {
        for matrix in matrices {
            result.push(Case {
                name,
                frame: [113, 79],
                matrix,
                svg: format!(
                    "<svg xmlns='http://www.w3.org/2000/svg' width='113' height='79'>{body}</svg>"
                ),
                rows: (0..79).collect(),
            });
        }
    }
    // The original renderer tiles at8191; preserve its f32 round trips, not
    // just a crop translated into a fresh local-coordinate canvas.
    for (name, frame, rows) in [
        ("horizontal-tiler", [8209, 19], vec![0, 1, 8, 9, 17, 18]),
        (
            "vertical-tiler",
            [19, 8209],
            vec![0, 1, 8189, 8190, 8191, 8192, 8208],
        ),
    ] {
        let [w, h] = frame;
        let body = format!("<defs><linearGradient id='l' x1='-.125' y1='.375' x2='{w}' y2='{h}' gradientUnits='userSpaceOnUse'><stop stop-color='#3b2aa7' stop-opacity='.37'/><stop offset='.423' stop-color='#f39421' stop-opacity='.81'/><stop offset='1' stop-color='#2aa78f' stop-opacity='.59'/></linearGradient></defs><g opacity='.83'><rect x='.375' y='.625' width='{}' height='{}' rx='2.375' fill='url(#l)'/><path d='M.125 1.375 L{} {} L2.375 {} Z' fill='#b4316a' fill-opacity='.43'/></g>",w as f32-1.25,h as f32-1.375,w as f32-0.625,h as f32-1.125,h as f32-0.375);
        result.push(Case {
            name,
            frame,
            matrix: [1., 0., 0., 1., 0.125, -0.125],
            svg: format!(
                "<svg xmlns='http://www.w3.org/2000/svg' width='{w}' height='{h}'>{body}</svg>"
            ),
            rows,
        });
    }
    for width in [1, 7, 8, 9, 15, 16, 17, 31, 32, 33] {
        result.push(Case{name:"horizontal-SIMD-tail",frame:[width,9],matrix:[1.,0.,0.,1.,0.,0.],rows:(0..9).collect(),svg:format!("<svg xmlns='http://www.w3.org/2000/svg' width='{width}' height='9'><defs><linearGradient id='g' x1='0' y1='0' x2='{width}' y2='9' gradientUnits='userSpaceOnUse'><stop stop-color='#f12c37' stop-opacity='.53'/><stop offset='1' stop-color='#218ab3' stop-opacity='.79'/></linearGradient></defs><rect width='{width}' height='9' fill='url(#g)'/><rect x='.25' y='.375' width='{width}' height='7.25' fill='#753bb1' fill-opacity='.31'/></svg>")});
    }
    result
}

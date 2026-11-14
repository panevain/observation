// Each colour constructor the check knows, one per line.
let a = Color32::from_rgb(1, 2, 3);
let b = Color32::from_rgba_unmultiplied(1, 2, 3, 4);
let c = Color32::from_rgba_premultiplied(1, 2, 3, 4);
let d = Color32::from_gray(128);
let e = Color::srgb(0.1, 0.2, 0.3);
let f = Color::srgba(0.1, 0.2, 0.3, 1.0);
let g = Color::rgb(0.1, 0.2, 0.3);
let h = Srgba::new(0.1, 0.2, 0.3, 1.0);
let i = Srgba::rgb(0.1, 0.2, 0.3);
let j = Color32::WHITE;
let k = Color32::LIGHT_GRAY;
let l = "#a1B2c3";
let m = format!("fill: {}", "#ffffffcc");
let n = ("x", "y #0099ff");
// Two on one line are still one line.
let o = (Color32::BLACK, Color::srgb(0.0, 0.0, 0.0));

// Near misses the check must not flag.
let p = Color::srgb_u8(1, 2, 3);
let q = Color::WHITE;
let r = Color32::from_hex("abc");
let s = Color32::default();
let t = "#abc";
let u = "#12345";
// A comment naming #aabbcc outside a string.
let v = theme::tokens().accent;
let w = color32_from_rgb(1, 2, 3);
let x = LinearRgba::new(0.1, 0.2, 0.3, 1.0);
let y = Srgba::hex("ffffff");
let z = 'x'; let hash = "#"; let six = "aabbcc";

// Flagged although the hex is outside the string: the pattern starts at any quote.
let quirk = "x"; // #aabbcc

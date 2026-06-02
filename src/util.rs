use gst;

pub const APP_ID: &str = "io.github.alisp.Soniq";

pub fn format_time(time: gst::ClockTime) -> String {
    let total = time.seconds();
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

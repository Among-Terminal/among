// Shared by runtime discovery and packaging; neither side may silently omit a
// dynamically discovered module just because it has no DT_NEEDED consumer.
pub const MEMBERS: [&str; 12] = [
    "gtk-4.0/4.0.0/media/libmedia-gstreamer.so",
    "gstreamer-1.0/libgstcoreelements.so",
    "gstreamer-1.0/libgstplayback.so",
    "gstreamer-1.0/libgsttypefindfunctions.so",
    "gstreamer-1.0/libgstaudioconvert.so",
    "gstreamer-1.0/libgstaudioresample.so",
    "gstreamer-1.0/libgstvolume.so",
    "gstreamer-1.0/libgstwavparse.so",
    "gstreamer-1.0/libgstpulseaudio.so",
    "gstreamer-1.0/libgstautodetect.so",
    "gstreamer-1.0/libgstaudiofx.so",
    "gstreamer1.0/gstreamer-1.0/gst-plugin-scanner",
];

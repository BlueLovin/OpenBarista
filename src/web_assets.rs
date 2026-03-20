pub struct StaticAsset {
    pub content_type: &'static str,
    pub cache_control: &'static str,
    pub body: &'static [u8],
}

const PORTAL_HTML_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/portal/index.html"
));
const PORTAL_CSS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/portal/portal.css"
));
const PORTAL_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/portal/portal.js"
));
const SUCCESS_HTML_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/portal/success.html"
));
const STATION_HTML_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/station/index.html"
));
const BASE_CSS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/station/base.css"
));
const SETTINGS_HTML_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/station/settings.html"
));
const STATION_CSS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/station/station.css"
));
const STATION_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/station/station.js"
));
const SETTINGS_CSS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/station/settings.css"
));
const SETTINGS_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/station/settings.js"
));
const UPLOT_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/station/uplot.min.js"
));
const UPLOT_CSS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/station/uplot.min.css"
));

pub fn captive_index_html(build_id: &str, board_id: &str) -> String {
    PORTAL_HTML_TEMPLATE
        .replace("{{BUILD_ID}}", build_id)
        .replace("{{BOARD_ID}}", board_id)
}

pub fn captive_success_html(build_id: &str, board_id: &str) -> String {
    SUCCESS_HTML_TEMPLATE
        .replace("{{BUILD_ID}}", build_id)
        .replace("{{BOARD_ID}}", board_id)
}

pub fn captive_static(path: &str) -> Option<StaticAsset> {
    match path {
        "/portal.css" => Some(StaticAsset {
            content_type: "text/css; charset=utf-8",
            cache_control: "public, max-age=86400",
            body: PORTAL_CSS,
        }),
        "/portal.js" => Some(StaticAsset {
            content_type: "application/javascript; charset=utf-8",
            cache_control: "public, max-age=86400",
            body: PORTAL_JS,
        }),
        _ => None,
    }
}

pub fn station_css() -> StaticAsset {
    StaticAsset {
        content_type: "text/css; charset=utf-8",
        cache_control: "public, max-age=86400",
        body: STATION_CSS,
    }
}

pub fn base_css() -> StaticAsset {
    StaticAsset {
        content_type: "text/css; charset=utf-8",
        cache_control: "public, max-age=86400",
        body: BASE_CSS,
    }
}

pub fn station_js() -> StaticAsset {
    StaticAsset {
        content_type: "application/javascript; charset=utf-8",
        cache_control: "public, max-age=86400",
        body: STATION_JS,
    }
}

pub fn settings_css() -> StaticAsset {
    StaticAsset {
        content_type: "text/css; charset=utf-8",
        cache_control: "public, max-age=86400",
        body: SETTINGS_CSS,
    }
}

pub fn settings_js() -> StaticAsset {
    StaticAsset {
        content_type: "application/javascript; charset=utf-8",
        cache_control: "public, max-age=86400",
        body: SETTINGS_JS,
    }
}

pub fn uplot_js() -> StaticAsset {
    StaticAsset {
        content_type: "application/javascript; charset=utf-8",
        cache_control: "public, max-age=604800",
        body: UPLOT_JS,
    }
}

pub fn uplot_css() -> StaticAsset {
    StaticAsset {
        content_type: "text/css; charset=utf-8",
        cache_control: "public, max-age=604800",
        body: UPLOT_CSS,
    }
}

pub fn station_index_html(ip_addr: &str, build_id: &str, board_id: &str) -> String {
    STATION_HTML_TEMPLATE
        .replace("{{IP_ADDR}}", ip_addr)
        .replace("{{BUILD_ID}}", build_id)
        .replace("{{BOARD_ID}}", board_id)
}

pub fn settings_index_html(ip_addr: &str, build_id: &str, board_id: &str) -> String {
    SETTINGS_HTML_TEMPLATE
        .replace("{{IP_ADDR}}", ip_addr)
        .replace("{{BUILD_ID}}", build_id)
        .replace("{{BOARD_ID}}", board_id)
}

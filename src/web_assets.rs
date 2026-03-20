pub struct StaticAsset {
    pub content_type: &'static str,
    pub cache_control: &'static str,
    pub body: &'static [u8],
}

const PORTAL_HTML: &[u8] = include_bytes!(concat!(
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
const SUCCESS_HTML: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/portal/success.html"
));
const STATION_HTML_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/station/index.html"
));
const STATION_CSS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/station/station.css"
));
const STATION_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/station/station.js"
));
const UPLOT_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/station/uplot.min.js"
));
const UPLOT_CSS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/station/uplot.min.css"
));

pub fn captive_index() -> StaticAsset {
    StaticAsset {
        content_type: "text/html; charset=utf-8",
        cache_control: "no-store",
        body: PORTAL_HTML,
    }
}

pub fn captive_success() -> StaticAsset {
    StaticAsset {
        content_type: "text/html; charset=utf-8",
        cache_control: "no-store",
        body: SUCCESS_HTML,
    }
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

pub fn station_js() -> StaticAsset {
    StaticAsset {
        content_type: "application/javascript; charset=utf-8",
        cache_control: "no-store",
        body: STATION_JS,
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

pub fn station_index_html(ip_addr: &str) -> String {
    STATION_HTML_TEMPLATE.replace("{{IP_ADDR}}", ip_addr)
}

use cef::{Browser, CefString, ImplBrowser as _, ImplFrame as _};

const LINE_PX: i32 = 80;

fn exec_js(browser: &Browser, code: &str) {
    let Some(frame) = browser.focused_frame().or_else(|| browser.main_frame()) else {
        return;
    };
    let code = CefString::from(code);
    let url = CefString::from("vmux://vim-scroll");
    frame.execute_java_script(Some(&code), Some(&url), 0);
}

pub fn scroll_line_down(browser: &Browser) {
    exec_js(browser, &format!("window.scrollBy(0,{LINE_PX});"));
}

pub fn scroll_line_up(browser: &Browser) {
    exec_js(browser, &format!("window.scrollBy(0,{});", -LINE_PX));
}

pub fn scroll_page_down(browser: &Browser) {
    exec_js(
        browser,
        "var h=window.innerHeight||document.documentElement.clientHeight||400;window.scrollBy(0,Math.max(1,Math.floor(h*0.9)));",
    );
}

pub fn scroll_page_up(browser: &Browser) {
    exec_js(
        browser,
        "var h=window.innerHeight||document.documentElement.clientHeight||400;window.scrollBy(0,-Math.max(1,Math.floor(h*0.9)));",
    );
}

pub fn scroll_top(browser: &Browser) {
    exec_js(browser, "window.scrollTo(0,0);");
}

pub fn scroll_bottom(browser: &Browser) {
    exec_js(
        browser,
        "var e=document.scrollingElement||document.documentElement||document.body;window.scrollTo(0,(e&&e.scrollHeight)||0);",
    );
}

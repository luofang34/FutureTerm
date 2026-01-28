use app_web::worker_logic::start_worker;

pub fn main() {
    console_error_panic_hook::set_once();
    #[cfg(debug_assertions)]
    {
        web_sys::console::log_1(&"WORKER STARTED".into());
        web_sys::console::log_1(&"WORKER VERSION 0.1.1-DEBUG".into());
    }
    wasm_bindgen_futures::spawn_local(start_worker());
}

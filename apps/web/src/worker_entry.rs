use app_web::worker_logic::start_worker;
use console_error_panic_hook;

pub fn main() {
    console_error_panic_hook::set_once();
    web_sys::console::log_1(&"WORKER STARTED".into());
    wasm_bindgen_futures::spawn_local(start_worker());
}

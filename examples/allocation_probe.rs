//! Development-only allocation probe; no network queries or device controls.
use homewire::{
    Result,
    fault::Monitor,
    klap::Session,
    mcp::{self, ToolSet},
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed},
};

struct CountingAllocator;
static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

// The probe is single-threaded. Every operation delegates the unchanged pointer
// and layout to System; instrumentation counts requested bytes, not live RSS.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Relaxed) {
            ALLOCATIONS.fetch_add(1, Relaxed);
            BYTES.fetch_add(layout.size(), Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if COUNTING.load(Relaxed) {
            ALLOCATIONS.fetch_add(1, Relaxed);
            BYTES.fetch_add(size, Relaxed);
        }
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn measure<T>(operation: impl FnOnce() -> T) -> (T, usize, usize) {
    ALLOCATIONS.store(0, Relaxed);
    BYTES.store(0, Relaxed);
    COUNTING.store(true, Relaxed);
    let result = operation();
    COUNTING.store(false, Relaxed);
    (result, ALLOCATIONS.load(Relaxed), BYTES.load(Relaxed))
}

fn main() -> Result<()> {
    let faults = vec!["overloaded".to_owned()];
    let mut monitor = Monitor::default();
    monitor.observe_faults("plug", "synthetic-endpoint", Some(&faults), 0);
    monitor.observe_faults("plug", "synthetic-endpoint", Some(&faults), 3);
    let (event, fault_allocations, fault_bytes) =
        measure(|| monitor.observe_faults("plug", "synthetic-endpoint", Some(&faults), 4));
    assert!(event.is_none());
    assert_eq!(
        fault_allocations, 0,
        "unchanged faults must allocate nothing"
    );

    let mut session = Session::new(&[1; 16], &[2; 16], &[3; 32])?;
    let (result, encrypt_allocations, encrypt_bytes) =
        measure(|| session.encrypt(br#"{"method":"get_device_info"}"#));
    let (mut frame, _) = result?;
    assert_eq!(encrypt_allocations, 1, "KLAP frames use one allocation");
    let (result, decrypt_allocations, decrypt_bytes) = measure(|| {
        session
            .decrypt_in_place(&mut frame)
            .map(|plain| plain.len())
    });
    assert_eq!(result?, br#"{"method":"get_device_info"}"#.len());
    assert_eq!(decrypt_allocations, 0, "KLAP responses reuse their storage");
    let request = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    let tools = ToolSet {
        meaco: true,
        printers: true,
        awair: true,
        bulbs: true,
        plugs: true,
        light_names: true,
        allow_control: true,
    };
    let (response, list_allocations, list_bytes) =
        measure(|| mcp::handle_configured(request, tools, |_, _| panic!("No device I/O")));
    assert_eq!(
        response.expect("response")["result"]["tools"]
            .as_array()
            .expect("tools")
            .len(),
        15
    );
    assert!(list_allocations <= 400, "MCP tool-list allocation budget");
    let mut lights: serde_json::Value = serde_json::from_str(include_str!("lights.json"))?;
    let mut plugs: serde_json::Value = serde_json::from_str(include_str!("plugs.json"))?;
    for (value, key) in [(&mut lights, "lights"), (&mut plugs, "plugs")] {
        for endpoint in value[key].as_array_mut().expect("endpoints") {
            endpoint["auth_hash"] = serde_json::json!("00".repeat(32));
        }
    }
    let lights: homewire::bulb::Config = serde_json::from_value(lights)?;
    let plugs: homewire::plug::Config = serde_json::from_value(plugs)?;
    let printers: homewire::printer::Config = serde_json::from_str(include_str!("printers.json"))?;
    let (result, validation_allocations, validation_bytes) = measure(|| -> Result<()> {
        lights.validate()?;
        plugs.validate()?;
        printers.validate()
    });
    result?;
    assert_eq!(
        validation_allocations, 0,
        "valid configuration checks use stack storage"
    );
    println!(
        "{{\"unchanged_fault\":{{\"allocations\":{fault_allocations},\"requested_bytes\":{fault_bytes}}},\"klap_encrypt\":{{\"allocations\":{encrypt_allocations},\"requested_bytes\":{encrypt_bytes}}},\"klap_decrypt_in_place\":{{\"allocations\":{decrypt_allocations},\"requested_bytes\":{decrypt_bytes}}},\"mcp_tools_list\":{{\"allocations\":{list_allocations},\"requested_bytes\":{list_bytes}}},\"config_validation\":{{\"allocations\":{validation_allocations},\"requested_bytes\":{validation_bytes}}}}}"
    );
    Ok(())
}

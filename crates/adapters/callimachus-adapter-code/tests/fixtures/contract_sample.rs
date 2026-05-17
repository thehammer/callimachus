/// A fixture file exercising every ContractSignals flag.
/// This file is NOT compiled into the test binary — it is read as text.

// ── panic risk ────────────────────────────────────────────────────────────────
pub fn risky_function(v: Vec<i32>) -> i32 {
    let first = v.first().unwrap();  // panic_call_count += 1, has_panic_risk
    let last = v.last().expect("empty vec");  // panic_call_count += 1
    first + last
}

// ── unsafe ────────────────────────────────────────────────────────────────────
pub fn unsafe_function(ptr: *const i32) -> i32 {
    unsafe { *ptr }  // has_unsafe
}

// ── must_use ──────────────────────────────────────────────────────────────────
#[must_use]
pub fn important_value() -> u64 {
    42
}

// ── deprecated ────────────────────────────────────────────────────────────────
#[deprecated(since = "0.2.0", note = "use new_api instead")]
pub fn old_api() -> String {
    "old".to_string()
}

// ── fallible / nullable / &mut self / public ──────────────────────────────────
pub struct MyStruct {
    value: i32,
}

impl MyStruct {
    pub fn fallible_method(&self) -> Result<i32, String> {  // is_fallible, is_public
        if self.value > 0 {
            Ok(self.value)
        } else {
            Err("negative".to_string())
        }
    }

    pub fn nullable_method(&self) -> Option<i32> {  // is_nullable, is_public
        if self.value > 0 { Some(self.value) } else { None }
    }

    pub fn mutating_method(&mut self) -> i32 {  // is_mutating, is_public
        self.value += 1;
        self.value
    }

    fn private_method(&self) -> i32 {  // NOT is_public
        self.value
    }
}

// ── debt markers ─────────────────────────────────────────────────────────────
pub fn debt_fn() {
    // TODO(phase-13): implement this properly
    // FIXME: this is broken
    // HACK: workaround for upstream bug
    let _ = "placeholder";
}

// ── is_incomplete ─────────────────────────────────────────────────────────────
pub fn not_implemented_yet() {
    unimplemented!("coming soon");
}

pub fn todo_fn() {
    todo!("write me");
}

// ── test function ─────────────────────────────────────────────────────────────
#[test]
fn test_fallible_method() {
    let s = MyStruct { value: 5 };
    assert!(s.fallible_method().is_ok());
    assert!(s.nullable_method().is_some());
}

// ── discards_result (let _ =) ─────────────────────────────────────────────────
pub fn discards_result_fn() {
    let _ = some_fallible_call();
    other_call().ok();
}

fn some_fallible_call() -> Result<(), String> {
    Ok(())
}

fn other_call() -> Result<(), String> {
    Ok(())
}

// ── diverging (never type) ────────────────────────────────────────────────────
pub fn diverging_fn() -> ! {
    panic!("always panics");
}

// ── branch_count ─────────────────────────────────────────────────────────────
pub fn branchy_fn(x: i32) -> &'static str {
    if x > 0 {
        match x {
            1 => "one",
            2 => "two",
            _ => "many",
        }
    } else if x < 0 {
        "negative"
    } else {
        "zero"
    }
}

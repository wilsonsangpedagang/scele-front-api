# Concurrency Issue Analysis & Fix — `scele-front-api`

---

## 1. Identifying the Concurrency Issues in `lib.rs` and `main.rs`

### What is the Critical Section of the Code?

The **critical section** is the block of code that reads, modifies, and writes the shared `request_count` variable. It is found in `main.rs` inside `get_all_announcements`:

```rust
// BEFORE FIX — critical section using UnsafeCell (unsafe, no synchronization)
unsafe {
    let request_count_ptr = data.request_count.get();  // Step 1: Get raw pointer
    let val = *request_count_ptr;                       // Step 2: Read current value
    let delay_ms = rand::thread_rng().gen_range(0..1000_u64); // Step 3: Simulate delay
    thread::sleep(Duration::from_millis(delay_ms));
    *request_count_ptr = val + 1;                       // Step 4: Write new value
}
```

This section is **critical** because:
- Multiple async Actix-web worker threads can execute `get_all_announcements` **concurrently**.
- They all share the **same `ServerState`** via `web::Data<ServerState>`.
- All threads read and write `request_count` without any locking mechanism.

---

### Is There a Race Condition in the Code?

**Yes.** A race condition exists because multiple threads access and modify `request_count` **without synchronization**.

**Before Fix — `ServerState` uses `UnsafeCell` (no thread safety):**

```rust
// BEFORE FIX
struct ServerState {
    pub request_count: UnsafeCell<u8>,
}

unsafe impl Sync for ServerState {} // Manually bypassing Rust's safety guarantees
```

`UnsafeCell` disables Rust's borrow checker protections for interior mutability but provides **zero synchronization**. The `unsafe impl Sync` tells the compiler "trust me, this is thread-safe" — but it is actually *not* thread-safe.

**Timeline illustrating the race condition:**

| Time | Thread A | Thread B | `request_count` |
|------|----------|----------|-----------------|
| t0   | reads val = 0 | | 0 |
| t1   | sleeps (delay) | reads val = 0 | 0 |
| t2   | sleeps (delay) | sleeps (delay) | 0 |
| t3   | writes 0 + 1 = 1 | | 1 |
| t4   | | writes 0 + 1 = 1 | 1 ← **Wrong! Should be 2** |

Two concurrent requests both read `0`, both compute `0 + 1 = 1`, and both write `1`. The counter ends at `1` instead of `2`.

---

### Is There a Lost Update in the Code?

**Yes.** The example above is a textbook **lost update**:

- Thread A's increment (`0 → 1`) is **overwritten** by Thread B's stale write (`0 → 1`).
- One of the two increments is permanently lost.
- This happens repeatedly under concurrent load, causing the final `request_count` to be **lower than the actual number of requests served**.

---

### How Does the Race Condition Correlate with the Lost Update?

The **race condition is the cause**; the **lost update is the effect**.

Because there is no synchronization (no lock), two threads can **interleave** their read-modify-write operations. The deliberate `thread::sleep` amplifies the interleaving window, making the problem easy to observe:

```
Thread A: READ(0) ──── SLEEP ──────────────────── WRITE(1)
Thread B:             READ(0) ──── SLEEP ──── WRITE(1)   ← overwrites Thread A's result
```

Without the race condition, a lost update would be impossible. The race condition creates the opportunity for a stale read, which inevitably leads to a lost write.

---

### What is the Synchronization Approach to Fix the Issue?

The fix replaces `UnsafeCell<u8>` with **`Mutex<u8>`** (from `std::sync`).

A `Mutex` ensures **mutual exclusion**: only one thread can hold the lock and access `request_count` at a time. All other threads block until the lock is released.

**After Fix — `ServerState` uses `Mutex`:**

```rust
// AFTER FIX
struct ServerState {
    pub request_count: Mutex<u8>,
}
// No need for `unsafe impl Sync` — Mutex<T> is Sync when T: Send
```

**After Fix — critical section uses `Mutex::lock()`:**

```rust
// AFTER FIX — critical section properly synchronized
{
    let mut count = data.request_count.lock().unwrap(); // Acquire lock
    let val = *count;
    let delay_ms = rand::thread_rng().gen_range(0..1000_u64);
    thread::sleep(Duration::from_millis(delay_ms));
    *count = val + 1;                                   // Write new value
    println!("Request count: {}", *count);
} // Lock is automatically released here (RAII)
```

**Timeline after fix:**

| Time | Thread A | Thread B | `request_count` |
|------|----------|----------|-----------------|
| t0   | acquires lock, reads val = 0 | tries to acquire lock, **blocks** | 0 |
| t1   | sleeps | blocked | 0 |
| t2   | writes 1, releases lock | unblocks, acquires lock, reads val = 1 | 1 |
| t3   | done | sleeps | 1 |
| t4   | | writes 2, releases lock | **2 ✓ Correct** |

---

### Is There a Possibility of Deadlock When Applying the Proposed Synchronization?

**No**, there is no deadlock risk in this specific implementation, for these reasons:

1. **Only one lock exists**: There is only a single `Mutex` protecting `request_count`. Deadlock requires at least two locks held in conflicting order (circular wait condition). With one lock, circular wait is impossible.

2. **Lock is always released**: Rust's `MutexGuard` uses RAII — the lock is automatically released when the guard goes out of scope, even if a panic occurs (though it poisons the mutex). There is no risk of forgetting to unlock.

3. **No nested locking**: The code does not attempt to acquire the same lock (or another lock) while already holding the mutex.

> **However**, a potential **lock poisoning** issue exists: if the thread panics while holding the lock, subsequent `lock().unwrap()` calls will panic too. This can be handled with `lock().unwrap_or_else(|e| e.into_inner())` if needed.

---

## 2. Before and After Fix — Full Code Comparison

### BEFORE FIX (`main.rs`)

```rust
use std::{cell::UnsafeCell, sync::Mutex, thread, time::Duration};
// ...

struct ServerState {
    pub request_count: UnsafeCell<u8>,  // No synchronization
}

unsafe impl Sync for ServerState {}     // Manually bypasses safety

#[get("/announcements")]
async fn get_all_announcements(data: web::Data<ServerState>) -> Result<impl Responder> {
    let page = get_frontpage("https://scele.cs.ui.ac.id").unwrap();
    let announcements = parse_frontpage(page);

    // Race condition: no lock, threads interleave freely
    unsafe {
        let request_count_ptr = data.request_count.get();
        let val = *request_count_ptr;
        let delay_ms = rand::thread_rng().gen_range(0..1000_u64);
        thread::sleep(Duration::from_millis(delay_ms));
        *request_count_ptr = val + 1;
    }

    println!("Request count: {}", unsafe { *data.request_count.get() });
    Ok(web::Json(announcements))
}

async fn main() -> std::io::Result<()> {
    let state = web::Data::new(ServerState {
        request_count: UnsafeCell::new(0),  //Starts with unsafe cell
    });
    // ...
}
```

**Problem summary:**
- `UnsafeCell<u8>` allows multiple threads to get a raw mutable pointer concurrently.
- No mutual exclusion: threads interleave the read-delay-write sequence.
- Result: `request_count` is almost always lower than the true number of requests.

---

### AFTER FIX (`main.rs`)

```rust
use std::{sync::Mutex, thread, time::Duration};  // Removed UnsafeCell
// ...

struct ServerState {
    pub request_count: Mutex<u8>,  // Mutex provides mutual exclusion
}

// No unsafe impl Sync needed — Mutex<T: Send> is already Sync

#[get("/announcements")]
async fn get_all_announcements(data: web::Data<ServerState>) -> Result<impl Responder> {
    let page = get_frontpage("https://scele.cs.ui.ac.id").unwrap();
    let announcements = parse_frontpage(page);

    // Only one thread at a time can enter this block
    {
        let mut count = data.request_count.lock().unwrap();
        let val = *count;
        let delay_ms = rand::thread_rng().gen_range(0..1000_u64);
        thread::sleep(Duration::from_millis(delay_ms));
        *count = val + 1;
        println!("Request count: {}", *count);
    } // Lock released automatically (RAII)

    Ok(web::Json(announcements))
}

async fn main() -> std::io::Result<()> {
    let state = web::Data::new(ServerState {
        request_count: Mutex::new(0),  // Protected by Mutex
    });
    // ...
}
```

**Fix summary:**
- `Mutex<u8>` guarantees that only one thread can read-modify-write `request_count` at a time.
- No `unsafe` code needed; Rust's type system enforces correctness.
- The delay inside the lock now makes threads wait in queue, so no update is ever lost.

---

## 3. Improvement — Response Cache Using `Mutex<Option<CachedResponse>>`

### Description

To reduce repeated network calls to SCELE (which is slow), we add a **cache** to `ServerState`. The first request fetches and parses the SCELE front page, stores the result, and subsequent requests use the cached copy directly.

### How Does the Improvement Affect the Shared Data in `ServerState`?

The improved `ServerState` now holds **two shared fields**:

```rust
struct CachedResponse {
    pub announcements: Vec<AnnouncementResponse>,
    pub cached_at: DateTime<Utc>,
}

struct ServerState {
    pub request_count: Mutex<u8>,
    pub cache: Mutex<Option<CachedResponse>>,  // New shared field
}
```

Both fields are wrapped in `Mutex`, so:
- `request_count` remains correctly synchronized as before.
- `cache` is also protected: only one thread can read or write the cache at a time.

**Access pattern for the cache:**

```rust
#[get("/announcements")]
async fn get_all_announcements(data: web::Data<ServerState>) -> Result<impl Responder> {
    // Check cache first
    let announcements = {
        let mut cache = data.cache.lock().unwrap();

        if let Some(ref cached) = *cache {
            // Cache hit: return stored announcements
            cached.announcements.clone()
        } else {
            // Cache miss: fetch from SCELE, store in cache
            let page = get_frontpage("https://scele.cs.ui.ac.id").unwrap();
            let fetched = parse_frontpage(page);
            *cache = Some(CachedResponse {
                announcements: fetched.clone(),
                cached_at: Utc::now(),
            });
            fetched
        }
    }; // Cache lock released

    // Increment request count (separate lock, no deadlock risk)
    {
        let mut count = data.request_count.lock().unwrap();
        *count += 1;
        println!("Request count: {}", *count);
    }

    Ok(web::Json(announcements))
}
```

---

### Is There a New Concurrency Issue with the Cache?

**No new race condition is introduced**, because the cache is protected by its own `Mutex`. However, there are two important considerations:

#### 1. Lock Ordering — Potential Deadlock Risk (mitigated by design)

With **two locks** (`request_count` and `cache`), there is a theoretical deadlock risk if:
- Thread A holds `cache` lock and tries to acquire `request_count` lock.
- Thread B holds `request_count` lock and tries to acquire `cache` lock.

In our implementation, this is **avoided** by **never holding both locks simultaneously**:
- The `cache` lock is acquired first, then released **before** acquiring `request_count`.
- The two `{ }` blocks are sequential, never nested.

#### 2. Cache Stampede (Minor Issue)

If many threads arrive simultaneously and the cache is empty (e.g., on startup), they all block on the `Mutex`. Only the first one fetches from SCELE; the rest wait. When the lock is released, they all see the populated cache and return immediately. This is correct behavior, just slightly slower on the very first burst.

#### 3. Cache Staleness (Known Trade-off)

The cache stores data indefinitely. If SCELE publishes new announcements, clients will receive stale data until the server restarts. This can be mitigated by adding a TTL (Time-To-Live) check using `cached_at`:

```rust
const CACHE_TTL_SECONDS: i64 = 60; // Cache valid for 60 seconds

if let Some(ref cached) = *cache {
    let age = Utc::now().signed_duration_since(cached.cached_at).num_seconds();
    if age < CACHE_TTL_SECONDS {
        return Ok(web::Json(cached.announcements.clone()));
    }
}
// Cache expired or empty — fetch fresh data
```

---

## Summary Table

| Aspect | Before Fix | After Fix | With Cache |
|--------|-----------|-----------|------------|
| `request_count` storage | `UnsafeCell<u8>` | `Mutex<u8>` | `Mutex<u8>` |
| Thread safety |  Manual + unsafe | Mutex | Mutex |
| Race condition | Yes (interleaving) | No | No |
| Lost update | Yes | No | No |
| Deadlock risk | N/A (no sync) | None (1 lock) | None (2 sequential locks) |
| Network calls to SCELE | Every request | Every request | Only on cache miss |
| New concurrency issues | — | None | Cache stampede (minor, by design) |

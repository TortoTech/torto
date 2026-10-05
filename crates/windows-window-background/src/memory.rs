use windows_sys::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// Returns the current process working set and private committed bytes.
pub fn process_memory() -> Option<(usize, usize)> {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    // SAFETY: the pseudo handle is valid for this process; the C-layout buffer
    // has the supplied size and GetProcessMemoryInfo initializes its fields.
    let success = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters as *mut _ as *mut _,
            counters.cb,
        )
    };
    (success != 0).then_some((counters.WorkingSetSize, counters.PrivateUsage))
}

/// Default Win32 heap entries, for opt-in diagnostics only. This excludes
/// separate driver heaps and direct VirtualAlloc/mapped allocations.
#[cfg(feature = "memory-profiling")]
#[derive(Default, Debug)]
pub struct ProcessHeapStats {
    pub busy_bytes: usize,
    pub free_bytes: usize,
    pub region_committed_bytes: usize,
    pub entries: usize,
}

#[cfg(feature = "memory-profiling")]
pub fn process_heap_stats() -> Option<ProcessHeapStats> {
    use windows_sys::Win32::Foundation::{ERROR_NO_MORE_ITEMS, GetLastError};
    use windows_sys::Win32::System::Memory::{
        GetProcessHeap, HeapLock, HeapUnlock, HeapWalk, PROCESS_HEAP_ENTRY,
    };
    const REGION: u16 = 0x1;
    const UNCOMMITTED: u16 = 0x2;
    const BUSY: u16 = 0x4;
    // SAFETY: the default process heap lives until process exit. Locking it
    // keeps HeapWalk entries stable. No allocation or callbacks occur while
    // locked; all paths after locking unlock it before returning.
    unsafe {
        let heap = GetProcessHeap();
        if heap.is_null() || HeapLock(heap) == 0 {
            return None;
        }
        let mut entry = PROCESS_HEAP_ENTRY::default();
        let mut stats = ProcessHeapStats::default();
        while HeapWalk(heap, &mut entry) != 0 {
            stats.entries += 1;
            if entry.wFlags & REGION != 0 {
                // The REGION flag identifies the initialized union member.
                stats.region_committed_bytes += entry.Anonymous.Region.dwCommittedSize as usize;
            } else if entry.wFlags & BUSY != 0 {
                stats.busy_bytes += entry.cbData as usize;
            } else if entry.wFlags & UNCOMMITTED == 0 {
                stats.free_bytes += entry.cbData as usize;
            }
        }
        let complete = GetLastError() == ERROR_NO_MORE_ITEMS;
        let unlocked = HeapUnlock(heap) != 0;
        (complete && unlocked).then_some(stats)
    }
}

#[cfg(all(test, feature = "memory-profiling"))]
mod tests {
    #[test]
    fn default_heap_walk_includes_live_system_allocations() {
        let allocation = vec![42_u8; 1024 * 1024];
        std::hint::black_box(&allocation);
        let stats = super::process_heap_stats().expect("default heap is available");
        assert!(stats.busy_bytes >= allocation.len());
        assert!(stats.entries > 0);
        assert_eq!(allocation[allocation.len() - 1], 42);
    }
}

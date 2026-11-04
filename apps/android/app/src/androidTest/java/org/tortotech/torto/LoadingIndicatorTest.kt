package org.tortotech.torto

import androidx.activity.compose.setContent
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.mutableStateOf
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

@RunWith(AndroidJUnit4::class)
class LoadingIndicatorTest {
    @Test fun shortOperationsNeverFlashAndSlowOperationsStopImmediately() {
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        val active=mutableStateOf(false)
        val visible=AtomicBoolean()
        val everVisible=AtomicBoolean()
        val shown=CountDownLatch(1)
        val finished=AtomicBoolean()
        val hidden=CountDownLatch(1)
        var activity:MainActivity?=null
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity
            assertNotNull(activity)
            instrumentation.runOnMainSync {activity!!.setContent {
                val show=loadingVisible(active.value)
                SideEffect {visible.set(show);if(show){everVisible.set(true);shown.countDown()}else if(finished.get())hidden.countDown()}
                MaterialTheme {if(show)LinearProgressIndicator()}
            }}
            instrumentation.waitForIdleSync()
            instrumentation.runOnMainSync {active.value=true}
            instrumentation.waitForIdleSync()
            android.os.SystemClock.sleep(50)
            instrumentation.runOnMainSync {active.value=false}
            instrumentation.waitForIdleSync()
            android.os.SystemClock.sleep(300)
            assertFalse("A completed cache hit must never publish a late loading frame",everVisible.get())
            instrumentation.runOnMainSync {active.value=true}
            assertTrue("Slow work must still get loading feedback",shown.await(3,TimeUnit.SECONDS))
            instrumentation.runOnMainSync {finished.set(true);active.value=false}
            assertTrue("Completion must remove the indicator on the next Compose update",hidden.await(3,TimeUnit.SECONDS))
            assertFalse("Completion must hide progress without a minimum display delay",visible.get())
        }finally {instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor)}
    }
}

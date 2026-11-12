package org.tortotech.torto

import android.os.SystemClock
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.BookSummary
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger

@RunWith(AndroidJUnit4::class)
class ReaderEntryTransitionTest {
    private fun await(message:String,condition:()->Boolean){
        val until=SystemClock.uptimeMillis()+5000
        while(!condition()){assertTrue(message,SystemClock.uptimeMillis()<until);SystemClock.sleep(20)}
    }
    @Test fun shelfExitsWhileReaderMountsAndIsReleasedAfterTheFade(){
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null
        val state=mutableStateOf(ScreenState(loaded=true))
        val shelf=AtomicBoolean();val reader=AtomicBoolean();val overlap=AtomicBoolean();val readerMounts=AtomicInteger()
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {
                activity!!.setContent {ReaderEntryHost(state.value){displayed->
                    val reading=displayed.active!=null
                    DisposableEffect(reading){
                        if(reading){reader.set(true);readerMounts.incrementAndGet()}else shelf.set(true)
                        onDispose {if(reading)reader.set(false)else shelf.set(false)}
                    }
                    SideEffect {if(shelf.get()&&reader.get())overlap.set(true)}
                    Box(Modifier.fillMaxSize().background(if(reading)Color.Blue else Color.Red))
                }}
            }
            await("The first shelf composition mounts"){shelf.get()&&!reader.get()}
            val book=BookSummary(0.0,0u,0u,"transition","Transition",emptyList())
            instrumentation.runOnMainSync {state.value=state.value.copy(active=book,openingBook=true)}
            await("The reader settles after the shelf exits"){reader.get()&&!shelf.get()}
            assertTrue("Opening mounts the reader while the previous shelf is still present",overlap.get())
            assertFalse("The outgoing shelf and its resources are released at the end",shelf.get())
            assertTrue(reader.get());assertEquals("The reader is not recreated at the end of the shelf fade",1,readerMounts.get())
            instrumentation.runOnMainSync {state.value=state.value.copy(active=null,openingBook=false)}
            await("Returning releases the reader composition"){shelf.get()&&!reader.get()}
            // Exit while opening is still animating: no delayed callback may reopen the reader.
            instrumentation.runOnMainSync {
                state.value=state.value.copy(active=book,openingBook=true)
                activity!!.window.decorView.postDelayed({state.value=state.value.copy(active=null,openingBook=false)},25)
            }
            SystemClock.sleep(300);instrumentation.waitForIdleSync();assertTrue(shelf.get());assertFalse(reader.get())
        }finally {instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor)}
    }
}

package org.tortotech.torto

import android.view.MotionEvent
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ReadingPanelDismissTest {
    @Test fun draggedPanelKeepsItsOffsetThroughExit() {
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        val open=mutableStateOf(true)
        var activity:MainActivity?=null
        fun await(message:String,condition:()->Boolean){
            val until=android.os.SystemClock.uptimeMillis()+5000
            while(!condition()){assertTrue(message,android.os.SystemClock.uptimeMillis()<until);android.os.SystemClock.sleep(20)}
        }
        fun top():Int?=instrumentation.uiAutomation.takeScreenshot()?.let {image->
            try {(0 until image.height).firstOrNull {y->image.getPixel(image.width/2,y).let {pixel->android.graphics.Color.green(pixel)>100&&android.graphics.Color.green(pixel)>android.graphics.Color.red(pixel)*1.1f&&android.graphics.Color.green(pixel)>android.graphics.Color.blue(pixel)*1.1f}}}
            finally {image.recycle()}
        }
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity
            assertNotNull(activity)
            instrumentation.runOnMainSync {activity!!.setContent {
                TortoTheme(ScreenState(appTheme="light",language="en")){
                    Box(Modifier.fillMaxSize().background(Color.Magenta)){
                        ReadingPanelHost("dismiss-test",if(open.value)"navigation"else null,{open.value=false}){
                            Box(Modifier.fillMaxSize().background(Color.Green))
                        }
                    }
                }
            }}
            await("panel enters"){top()!=null}
            android.os.SystemClock.sleep(300)
            val before=top()!!
            val width=activity!!.window.decorView.width.toFloat()
            val height=activity!!.window.decorView.height.toFloat()
            val start=android.os.SystemClock.uptimeMillis()
            fun event(action:Int,y:Float){MotionEvent.obtain(start,android.os.SystemClock.uptimeMillis(),action,width/2,y,0).also {instrumentation.sendPointerSync(it);it.recycle()}}
            event(MotionEvent.ACTION_DOWN,before+40f)
            repeat(8){step->android.os.SystemClock.sleep(16);event(MotionEvent.ACTION_MOVE,before+40f+height*.045f*(step+1))}
            android.os.SystemClock.sleep(100)
            val dragged=top()!!
            assertTrue("the panel follows the finger: before=$before, dragged=$dragged, height=$height",dragged>before+height*.2f)
            event(MotionEvent.ACTION_UP,before+40f+height*.36f)
            await("release dismisses"){!open.value}
            // Sample immediately during fade-out: retaining drag prevents a
            // jump back to the original top before the sheet disappears.
            top()?.let {assertTrue("exit must never jump upward: $dragged -> $it",it>=dragged-2)}
            await("exit completes"){top()==null}
        }finally {
            instrumentation.runOnMainSync {activity?.finish()}
            instrumentation.removeMonitor(monitor)
        }
    }
}

package org.tortotech.torto

import android.graphics.Color
import android.graphics.Rect
import android.view.MotionEvent
import android.view.accessibility.AccessibilityNodeInfo
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class IconAppearanceTest {
    @Test fun pressedAndActiveButtonsShareTheRoundedOutline(){
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null
        var down:MotionEvent?=null
        fun find(node:AccessibilityNodeInfo?,label:String):AccessibilityNodeInfo? {
            if(node==null)return null
            if(node.contentDescription?.toString()==label)return node
            return (0 until node.childCount).firstNotNullOfOrNull {find(node.getChild(it),label)}
        }
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {activity!!.setContent {
                TortoTheme(ScreenState(appTheme="light",language="en")){
                    Row(Modifier.fillMaxSize().background(androidx.compose.ui.graphics.Color.White).padding(start=24.dp,top=100.dp),horizontalArrangement=Arrangement.spacedBy(16.dp)){
                        ActionIcon("search","Press outline"){}
                        ActionIcon("search","Active outline",selected=true){}
                    }
                }
            }}
            instrumentation.waitForIdleSync();android.os.SystemClock.sleep(200)
            fun bounds(label:String):Rect {
                var button=find(instrumentation.uiAutomation.rootInActiveWindow,label)
                while(button!=null&&!button.isClickable)button=button.parent
                assertNotNull(button)
                return Rect().also {button!!.getBoundsInScreen(it)}
            }
            val pressed=bounds("Press outline")
            val active=bounds("Active outline")
            val time=android.os.SystemClock.uptimeMillis()
            down=MotionEvent.obtain(time,time,MotionEvent.ACTION_DOWN,pressed.exactCenterX(),pressed.exactCenterY(),0)
            instrumentation.sendPointerSync(down)
            android.os.SystemClock.sleep(350)
            val bitmap=instrumentation.uiAutomation.takeScreenshot();assertNotNull(bitmap)
            java.io.File(instrumentation.targetContext.cacheDir,"icon-pressed.png").outputStream().use {bitmap!!.compress(android.graphics.Bitmap.CompressFormat.PNG,100,it)}
            val density=instrumentation.targetContext.resources.displayMetrics.density
            assertEquals(48f,pressed.width()/density,1f)
            fun pixel(rect:Rect,dp:Int)=bitmap!!.getPixel(rect.left+(dp*density).toInt(),rect.top+(dp*density).toInt())
            fun difference(color:Int)=kotlin.math.abs(Color.red(color)-255)+kotlin.math.abs(Color.green(color)-255)+kotlin.math.abs(Color.blue(color)-255)
            assertTrue("Pressed ripple must reach the rounded-rectangle shoulder instead of using a circle",difference(pixel(pressed,7))>8)
            assertTrue("Active fill reaches the same shoulder",difference(pixel(active,7))>8)
            assertTrue("Pressed indication stays outside the rounded corner",difference(pixel(pressed,1))<8)
            assertTrue("Active fill stays outside the same rounded corner",difference(pixel(active,1))<8)
            bitmap!!.recycle()
        }finally {
            down?.let {event->instrumentation.sendPointerSync(MotionEvent.obtain(event.downTime,android.os.SystemClock.uptimeMillis(),MotionEvent.ACTION_UP,event.x,event.y,0));event.recycle()}
            instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor)
        }
    }
}

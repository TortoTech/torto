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
    @Test fun pressedAndActiveButtonsOnlyChangeTheSameIconInk(){
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
                        ActionIcon("toc","Normal ink"){}
                        ActionIcon("toc","Press ink"){}
                        ActionIcon("toc","Active ink",selected=true){}
                    }
                }
            }}
            // Accessibility reports animated window transforms during launch;
            // sample hit rectangles only after that finite transition settles.
            instrumentation.waitForIdleSync();android.os.SystemClock.sleep(700)
            fun bounds(label:String):Rect {
                if(android.os.Build.VERSION.SDK_INT>=33)instrumentation.uiAutomation.clearCache()
                var button=find(instrumentation.uiAutomation.rootInActiveWindow,label)
                while(button!=null&&!button.isClickable)button=button.parent
                assertNotNull(button)
                return Rect().also {button!!.getBoundsInScreen(it)}
            }
            val normal=bounds("Normal ink")
            val pressed=bounds("Press ink")
            val active=bounds("Active ink")
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
            for(rect in listOf(normal,pressed,active)){
                assertTrue("Icons have no background at the former rounded shoulder",difference(pixel(rect,7))<3)
                assertTrue("Transparent targets keep their corners empty",difference(pixel(rect,1))<3)
                assertEquals(normal.width(),rect.width());assertEquals(normal.height(),rect.height())
            }
            var inkPixels=0;var changed=0;var maskDifference=0
            for(y in 0 until normal.height())for(x in 0 until normal.width()){
                val base=bitmap!!.getPixel(normal.left+x,normal.top+y)
                val press=bitmap.getPixel(pressed.left+x,pressed.top+y)
                val selected=bitmap.getPixel(active.left+x,active.top+y)
                val delta=kotlin.math.abs(Color.red(press)-Color.red(selected))+kotlin.math.abs(Color.green(press)-Color.green(selected))+kotlin.math.abs(Color.blue(press)-Color.blue(selected))
                assertTrue("Pressed and active differ only by subpixel colour rounding",delta<=6)
                if(difference(base)>20){inkPixels++;if(base!=press)changed++}
                if((difference(base)>8)!=(difference(press)>8))maskDifference++
            }
            assertTrue("The vector itself changes colour",inkPixels>100&&changed>inkPixels*.8)
            assertTrue("Colour feedback cannot fill, move or scale the icon",maskDifference<inkPixels*.05)
            bitmap!!.recycle()
        }finally {
            down?.let {event->instrumentation.sendPointerSync(MotionEvent.obtain(event.downTime,android.os.SystemClock.uptimeMillis(),MotionEvent.ACTION_UP,event.x,event.y,0));event.recycle()}
            instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor)
        }
    }
}

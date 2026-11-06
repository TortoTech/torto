package org.tortotech.torto

import android.graphics.Bitmap
import android.graphics.Color
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.view.accessibility.AccessibilityNodeInfo
import android.view.inspector.WindowInspector
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.core.view.WindowInsetsCompat
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class MediaPreviewTest {
    @Test fun imageUsesBlackFullScreenAndRestoresReaderAfterBlankTapAndBack(){
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null
        val open=mutableStateOf(false);val dark=mutableStateOf(false)
        val green=0xff35a177.toInt()
        val bitmap=Bitmap.createBitmap(320,144,Bitmap.Config.ARGB_8888).apply {eraseColor(green)}
        fun await(message:String,condition:()->Boolean){
            val until=android.os.SystemClock.uptimeMillis()+5000
            while(!condition()){assertTrue(message,android.os.SystemClock.uptimeMillis()<until);android.os.SystemClock.sleep(25)}
        }
        fun image(view:View):ZoomImageView?=if(view is ZoomImageView&&view.isAttachedToWindow)view
            else if(view is ViewGroup)(0 until view.childCount).firstNotNullOfOrNull {image(view.getChildAt(it))}else null
        fun preview():ZoomImageView? {
            var result:ZoomImageView?=null
            instrumentation.runOnMainSync {result=WindowInspector.getGlobalWindowViews().firstNotNullOfOrNull(::image)}
            return result
        }
        fun labelled(node:AccessibilityNodeInfo?,labels:Set<String>):Boolean {
            if(node==null)return false
            if(node.text?.toString() in labels||node.contentDescription?.toString() in labels)return true
            return (0 until node.childCount).any {labelled(node.getChild(it),labels)}
        }
        fun tap(x:Float,y:Float){
            val time=android.os.SystemClock.uptimeMillis()
            for(action in listOf(MotionEvent.ACTION_DOWN,MotionEvent.ACTION_UP))MotionEvent.obtain(time,android.os.SystemClock.uptimeMillis(),action,x,y,0).also {instrumentation.sendPointerSync(it);it.recycle()}
        }
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {activity!!.setContent {
                TortoTheme(ScreenState(appTheme=if(dark.value)"dark"else"light",language="en")){
                    Box(Modifier.fillMaxSize().background(androidx.compose.ui.graphics.Color.Magenta)){
                        if(open.value)FullScreenImagePreview(bitmap){open.value=false}
                    }
                }
            }}
            instrumentation.waitForIdleSync()
            fun statusVisible(view:View)=WindowInsetsCompat.toWindowInsetsCompat(view.rootWindowInsets).isVisible(WindowInsetsCompat.Type.statusBars())
            val previousStatus=statusVisible(activity!!.window.decorView)
            for(theme in listOf(false,true)){
                instrumentation.runOnMainSync {dark.value=theme;open.value=true}
                await("image owns a fullscreen window"){preview()?.let {it.hasWindowFocus()&&it.width>0&&it.height>0&&!statusVisible(it)}==true}
                android.os.SystemClock.sleep(350)
                val view=preview()!!
                val location=IntArray(2);instrumentation.runOnMainSync {view.getLocationOnScreen(location)}
                assertArrayEquals("Image starts at the screen origin",intArrayOf(0,0),location)
                val shot=instrumentation.uiAutomation.takeScreenshot()!!
                try {
                    assertEquals(shot.width,view.width);assertEquals(shot.height,view.height)
                    assertEquals("Top letterbox is black in either theme",Color.BLACK,shot.getPixel(shot.width/4,shot.height/8))
                    assertEquals("Bottom letterbox is black",Color.BLACK,shot.getPixel(shot.width/4,shot.height*7/8))
                    assertEquals("Image stays centred at its original aspect ratio",green,shot.getPixel(shot.width/2,shot.height/2))
                    File(instrumentation.targetContext.cacheDir,"image-preview-${if(theme)"dark"else"light"}.png").outputStream().use {shot.compress(Bitmap.CompressFormat.PNG,100,it)}
                }finally {shot.recycle()}
                if(android.os.Build.VERSION.SDK_INT>=33)instrumentation.uiAutomation.clearCache()
                assertFalse("No header actions",labelled(instrumentation.uiAutomation.rootInActiveWindow,setOf("询问这张图","Ask about this image","关闭预览","Close preview")))
                tap(view.width/2f,view.height/2f)
                android.os.SystemClock.sleep(400)
                assertTrue("A tap on the image must not dismiss it",open.value)
                tap(view.width/4f,view.height/8f)
                await("a confirmed blank tap closes the image"){!open.value&&preview()==null}
                await("closing restores the reader system bars"){statusVisible(activity!!.window.decorView)==previousStatus}
                instrumentation.runOnMainSync {open.value=true}
                await("preview can reopen"){preview()?.hasWindowFocus()==true}
                android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("input keyevent 4")).use {it.readBytes()}
                await("system back closes without a visible button"){!open.value&&preview()==null}
            }
        }finally {
            instrumentation.runOnMainSync {open.value=false;activity?.finish()}
            instrumentation.removeMonitor(monitor);bitmap.recycle()
        }
    }
}

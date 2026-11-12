package org.tortotech.torto

import android.graphics.Bitmap
import android.graphics.Rect
import android.os.SystemClock
import android.view.accessibility.AccessibilityNodeInfo
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.*
import androidx.compose.ui.Modifier
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.SelectedText
import org.tortotech.torto.core.Rect as TextRect
import java.io.File

@RunWith(AndroidJUnit4::class)
class SelectionToolbarTest {
    @Test fun selectionSurfaceHasThreeLabeledActionsAndNoAiOrClose(){
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null
        fun node(predicate:(AccessibilityNodeInfo)->Boolean):AccessibilityNodeInfo? {
            instrumentation.uiAutomation.clearCache()
            fun visit(n:AccessibilityNodeInfo?):AccessibilityNodeInfo? {if(n==null)return null;if(n.isVisibleToUser&&predicate(n))return n;for(i in 0 until n.childCount)visit(n.getChild(i))?.let {return it};return null}
            return visit(instrumentation.uiAutomation.rootInActiveWindow)
        }
        fun label(text:String)=node {it.text?.toString()==text||it.contentDescription?.toString()==text}
        fun await(condition:()->Boolean){val end=SystemClock.uptimeMillis()+8000;while(!condition()){assertTrue(SystemClock.uptimeMillis()<end);SystemClock.sleep(30)}}
        var copied=0;var highlighted=0;var noted=0
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(8000) as MainActivity
            instrumentation.runOnMainSync {activity!!.setContent {TortoTheme(ScreenState(appTheme="light",language="en")){
                Box(Modifier.fillMaxSize()){
                    SelectionToolbar(SelectedText("Selected passage",listOf(TextRect(24f,240f,280f,24f)),false),
                        onCopy={copied++},onDismiss={},onHighlight={highlighted++},onNote={noted++})
                }
            }}}
            await {label("Copy")!=null&&label("Highlight")!=null&&label("Note")!=null}
            val pane=node {it.paneTitle?.toString()=="Selection actions"}!!
            val bounds=Rect().also(pane::getBoundsInScreen)
            val density=instrumentation.targetContext.resources.displayMetrics.density
            assertEquals(64f,bounds.height()/density,1f)
            assertEquals(216f,bounds.width()/density,1f)
            for(text in listOf("Clear selection","Ask","Reading assistant"))assertNull(text,label(text))
            val rects=listOf("Copy","Highlight","Note").map {text->
                var button=label(text);while(button!=null&&!button.isClickable)button=button.parent
                assertNotNull(button)
                val rect=Rect().also(button!!::getBoundsInScreen)
                assertTrue(rect.width()/density>=48f&&rect.height()/density>=48f)
                assertTrue(button.performAction(AccessibilityNodeInfo.ACTION_CLICK));rect
            }
            assertTrue(rects[0].right<=rects[1].left&&rects[1].right<=rects[2].left)
            assertEquals(1,copied);assertEquals(1,highlighted);assertEquals(1,noted)
            instrumentation.uiAutomation.takeScreenshot()!!.let {image->
                try {
                    assertEquals(android.graphics.Color.rgb(61,61,61),image.getPixel(bounds.left+(8*density).toInt(),bounds.centerY()))
                    File(instrumentation.targetContext.cacheDir,"selection-toolbar-labelled.png").outputStream().use {image.compress(Bitmap.CompressFormat.PNG,100,it)}
                }finally {image.recycle()}
            }
        }finally {instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor)}
    }
}

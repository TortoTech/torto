package org.tortotech.torto

import android.graphics.Rect
import android.os.SystemClock
import android.view.MotionEvent
import android.view.accessibility.AccessibilityNodeInfo
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.Text
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.ui.unit.dp
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ReaderFeedbackTest {
    private fun find(node:AccessibilityNodeInfo?,text:String):AccessibilityNodeInfo? {
        if(node==null)return null
        if(node.isVisibleToUser&&(node.text?.toString()==text||node.contentDescription?.toString()==text))return node
        return (0 until node.childCount).firstNotNullOfOrNull {find(node.getChild(it),text)}
    }
    @Test fun translatedSelectionUsesTooltipAndErrorsDisappearWithoutRetrying(){
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        val state=mutableStateOf(ScreenState(language="zh-CN",translated=true,chromeVisible=true))
        var opened=0
        var activity:MainActivity?=null
        val service=instrumentation.uiAutomation.serviceInfo
        service.flags=service.flags or android.accessibilityservice.AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS
        instrumentation.uiAutomation.serviceInfo=service
        fun node(text:String):AccessibilityNodeInfo? {
            if(android.os.Build.VERSION.SDK_INT>=33)instrumentation.uiAutomation.clearCache()
            return find(instrumentation.uiAutomation.rootInActiveWindow,text) ?: instrumentation.uiAutomation.windows.firstNotNullOfOrNull {find(it.root,text)}
        }
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {activity!!.setContent {
                TortoTheme(state.value){Box(Modifier.fillMaxSize()){
                    ReadingChrome(state.value,false,null,{}, {opened++},{},{})
                    ReaderAiNotice(state.value,Modifier.align(androidx.compose.ui.Alignment.TopCenter).padding(top=readerSafePadding().calculateTopPadding()+12.dp))
                }}
            }}
            // Launch transitions transform accessibility bounds even after the
            // composition is idle. Sample touch coordinates once they settle.
            instrumentation.waitForIdleSync();SystemClock.sleep(700)
            val button=node("文字选择模式");assertNotNull(button)
            val bounds=Rect();button!!.getBoundsInScreen(bounds)
            val down=SystemClock.uptimeMillis()
            instrumentation.sendPointerSync(MotionEvent.obtain(down,down,MotionEvent.ACTION_DOWN,bounds.exactCenterX(),bounds.exactCenterY(),0))
            instrumentation.sendPointerSync(MotionEvent.obtain(down,down+50,MotionEvent.ACTION_UP,bounds.exactCenterX(),bounds.exactCenterY(),0))
            instrumentation.waitForIdleSync();SystemClock.sleep(150)
            assertNotNull("Disabled selection must explain the restriction",node("翻译时仅支持按段选择"))
            assertEquals("The tooltip must not open the selection sheet",0,opened)
            instrumentation.uiAutomation.takeScreenshot()?.let {bitmap->java.io.File(instrumentation.targetContext.cacheDir,"reader-selection-tooltip.png").outputStream().use {bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG,100,it)};bitmap.recycle()}
            SystemClock.sleep(2100)
            instrumentation.runOnMainSync {state.value=state.value.copy(aiError="private provider details",aiErrorKind="translation",aiErrorEvent=1u,aiNoticeUntil=SystemClock.uptimeMillis()+2000)}
            instrumentation.waitForIdleSync();SystemClock.sleep(180)
            assertNotNull(node("翻译服务错误"));assertNull(node("private provider details"))
            val image=instrumentation.uiAutomation.takeScreenshot()
            java.io.File(instrumentation.targetContext.cacheDir,"reader-translation-notice.png").outputStream().use {image!!.compress(android.graphics.Bitmap.CompressFormat.PNG,100,it)}
            image?.recycle()
            SystemClock.sleep(2200);instrumentation.waitForIdleSync()
            assertNull("A terminal translation notice lasts two seconds",node("翻译服务错误"))
            instrumentation.runOnMainSync {state.value=state.value.copy(processing=true)}
            instrumentation.waitForIdleSync();SystemClock.sleep(180)
            assertNull("Unrelated status updates cannot repeat the same failure",node("翻译服务错误"))
        } finally {instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor)}
    }
    @Test fun listConsumesBodyDragUntilItReachesTheTop(){
        val instrumentation=InstrumentationRegistry.getInstrumentation()
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        val top=mutableStateOf(false)
        val panel=mutableStateOf<String?>("navigation")
        var list:LazyListState?=null
        var dismissed=0
        var activity:MainActivity?=null
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {activity!!.setContent {
                TortoTheme(ScreenState(appTheme="light")){
                    Box(Modifier.fillMaxSize()){
                        ReadingPanelHost("gesture-fixture",panel.value,{dismissed++;panel.value=null}){
                            val scrolling=rememberLazyListState(20)
                            SideEffect {list=scrolling}
                            LaunchedEffect(top.value){if(top.value)scrolling.scrollToItem(0)}
                            LazyColumn(Modifier.fillMaxSize(),state=scrolling){items(100){Text("Chapter $it",Modifier.fillMaxWidth().height(64.dp).padding(16.dp))}}
                        }
                    }
                }
            }}
            instrumentation.waitForIdleSync();SystemClock.sleep(300)
            val density=instrumentation.targetContext.resources.displayMetrics.density
            fun drag(distance:Float){
                val start=SystemClock.uptimeMillis();val x=160*density;val y=250*density
                instrumentation.sendPointerSync(MotionEvent.obtain(start,start,MotionEvent.ACTION_DOWN,x,y,0))
                for(step in 1..24){SystemClock.sleep(15);instrumentation.sendPointerSync(MotionEvent.obtain(start,SystemClock.uptimeMillis(),MotionEvent.ACTION_MOVE,x,y+distance*density*step/24,0))}
                instrumentation.sendPointerSync(MotionEvent.obtain(start,SystemClock.uptimeMillis(),MotionEvent.ACTION_UP,x,y+distance*density,0))
                instrumentation.waitForIdleSync();SystemClock.sleep(350)
            }
            drag(120f)
            assertEquals("Scrolling inside the list cannot dismiss the panel",0,dismissed)
            assertTrue("The list owns the drag while away from the top",list!!.firstVisibleItemIndex<20&&list!!.firstVisibleItemIndex>0)
            instrumentation.runOnMainSync {top.value=true}
            instrumentation.waitForIdleSync();SystemClock.sleep(200)
            assertEquals(0,list!!.firstVisibleItemIndex)
            drag(300f)
            assertEquals("A downward body drag at the list top moves and dismisses the panel",1,dismissed)
        }finally {instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor)}
    }

}

package org.tortotech.torto

import android.os.SystemClock
import android.view.accessibility.AccessibilityNodeInfo
import androidx.lifecycle.ViewModelProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.*
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.atomic.AtomicBoolean

@RunWith(AndroidJUnit4::class)
class TypographyEntryTest {
    private val instrumentation get()=InstrumentationRegistry.getInstrumentation()
    private fun await(message:String,condition:()->Boolean){
        val until=SystemClock.uptimeMillis()+8000
        while(!condition()){assertTrue(message,SystemClock.uptimeMillis()<until);SystemClock.sleep(20)}
    }
    private fun label(text:String):AccessibilityNodeInfo? {
        instrumentation.uiAutomation.clearCache()
        fun visit(node:AccessibilityNodeInfo?):AccessibilityNodeInfo? {
            if(node==null)return null
            if(node.isVisibleToUser&&(node.text?.toString()==text||node.contentDescription?.toString()==text))return node
            for(i in 0 until node.childCount)visit(node.getChild(i))?.let {return it}
            return null
        }
        return visit(instrumentation.uiAutomation.rootInActiveWindow)
    }
    private fun click(text:String){
        await("$text appears"){label(text)!=null}
        var node=label(text)
        while(node!=null&&!node.isClickable)node=node.parent
        assertNotNull("$text is actionable",node);assertTrue(node!!.performAction(AccessibilityNodeInfo.ACTION_CLICK))
    }
    @Test fun coldAndWarmTypographyEntryKeepControlsEnabledDuringMetadataLoading(){
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        val scope=CoroutineScope(SupervisorJob()+Dispatchers.Unconfined)
        var activity:MainActivity?=null;var model:ReaderModel?=null;var original:ScreenState?=null
        val watching=AtomicBoolean();val trace=ConcurrentLinkedQueue<String>()
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java]}
            await("model initializes"){model!!.state.value.loaded&&!model!!.state.value.busy};original=model!!.state.value
            instrumentation.runOnMainSync {model!!.interfaceSettings("light","en")}
            await("English applies"){model!!.state.value.language=="en"&&!model!!.state.value.busy}
            scope.launch {model!!.state.collect {if(watching.get())trace.add("busy=${it.busy} fonts=${it.fontFamilies!=null}")}}
            click("Me");click("Settings")
            repeat(2){attempt->
                trace.clear();watching.set(true)
                click("Typesetting")
                await("font choices are available"){model!!.state.value.fontFamilies!=null&&!model!!.state.value.busy&&label("Latin font")!=null}
                SystemClock.sleep(250);watching.set(false)
                File(instrumentation.targetContext.cacheDir,"typography-entry-$attempt.txt").writeText(trace.joinToString("\n"))
                assertTrue("Opening metadata must not grey out the entire page: $trace",trace.none {it.startsWith("busy=true")})
                assertEquals(original!!.reading,model!!.state.value.reading)
                val savedSize=model!!.state.value.fontSize
                val increase=savedSize<40f
                click(if(increase)"Increase font size"else "Decrease font size")
                await("shared capsule saves the settings-page font size"){!model!!.state.value.busy&&model!!.state.value.fontSize==savedSize+if(increase)2f else -2f}
                assertEquals(model!!.state.value.fontSize,instrumentation.targetContext.getSharedPreferences("reading",0).getFloat("size",0f))
                click(if(increase)"Decrease font size"else "Increase font size")
                await("the settings-page font size restores"){!model!!.state.value.busy&&model!!.state.value.fontSize==savedSize}
                assertNotNull("Global typesetting exposes the saved focus preference",label("Focus mode"))
                val focus=model!!.state.value.focus
                assertEquals("Only classic mode exposes the unified-layout preference",!focus,label("Unified layout")!=null)
                click("Focus mode")
                await("focus preference saves without an open book"){model!!.state.value.focus!=focus&&!model!!.state.value.busy}
                await("layout switch follows the focus mode"){(label("Unified layout")!=null)==focus}
                assertEquals(!focus,instrumentation.targetContext.getSharedPreferences("reading",0).getBoolean("focus",focus))
                click("Focus mode")
                await("focus preference restores"){model!!.state.value.focus==focus&&!model!!.state.value.busy}
                await("restored mode shows the matching controls"){(label("Unified layout")!=null)==!focus}
                click("Latin font");await("font selector opens"){label("Search")!=null}
                click("Back");await("typesetting remains present"){label("General")!=null}
                click("Back");await("settings returns"){label("Cloud sync")!=null}
            }
        }finally {
            watching.set(false);scope.cancel()
            original?.let {saved->instrumentation.runOnMainSync {model!!.configure(saved.fontSize,saved.unified,saved.dark);model!!.focus(saved.focus)};await("focus restores"){!model!!.state.value.busy&&model!!.state.value.focus==saved.focus&&model!!.state.value.fontSize==saved.fontSize}}
            original?.let {saved->instrumentation.runOnMainSync {model!!.interfaceSettings(saved.appTheme,saved.language)};await("interface restores"){!model!!.state.value.busy}}
            instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor)
        }
    }
}

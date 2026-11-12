package org.tortotech.torto

import android.graphics.Bitmap
import android.graphics.Rect
import android.os.Bundle
import android.os.SystemClock
import android.view.accessibility.AccessibilityNodeInfo
import androidx.lifecycle.ViewModelProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

@RunWith(AndroidJUnit4::class)
class HighlightSelectionTest {
    private val instrumentation get()=InstrumentationRegistry.getInstrumentation()
    private fun node(predicate:(AccessibilityNodeInfo)->Boolean):AccessibilityNodeInfo? {
        instrumentation.uiAutomation.clearCache()
        fun visit(n:AccessibilityNodeInfo?):AccessibilityNodeInfo? {if(n==null)return null;if(n.isVisibleToUser&&predicate(n))return n;for(i in 0 until n.childCount)visit(n.getChild(i))?.let {return it};return null}
        return visit(instrumentation.uiAutomation.rootInActiveWindow)
    }
    private fun label(value:String)=node {it.text?.toString()==value||it.contentDescription?.toString()==value}
    private fun await(message:String,condition:()->Boolean){val end=SystemClock.uptimeMillis()+16000;while(!condition()){if(SystemClock.uptimeMillis()>=end){shot("highlight-selection-failure.png");fail(message)};SystemClock.sleep(30)}}
    private fun click(text:String){
        var target:AccessibilityNodeInfo?=null
        await("$text is enabled") {var n=label(text);while(n!=null&&!n.isClickable)n=n.parent;target=n;n?.isEnabled==true}
        assertTrue(target!!.performAction(AccessibilityNodeInfo.ACTION_CLICK));instrumentation.waitForIdleSync()
    }
    private fun shot(name:String){instrumentation.uiAutomation.takeScreenshot()!!.let {image->try{File(instrumentation.targetContext.cacheDir,name).outputStream().use {image.compress(Bitmap.CompressFormat.PNG,100,it)}}finally{image.recycle()}}}
    @Test fun highlightsToggleMergeAndKeepIndependentNotesWithCompactComposer():Unit=runBlocking {
        val context=instrumentation.targetContext;val core=NativeRuntime.core(context)
        val service=instrumentation.uiAutomation.serviceInfo
        service.flags=service.flags or android.accessibilityservice.AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS
        instrumentation.uiAutomation.serviceInfo=service
        fun keyboard()=instrumentation.uiAutomation.windows.firstOrNull {it.type==android.view.accessibility.AccessibilityWindowInfo.TYPE_INPUT_METHOD}
        val ai=core.aiOptions();val reading=core.readingOptions();val prefs=context.getSharedPreferences("reading",0);val oldPrefs=prefs.all
        val file=File(context.cacheDir,"highlight-selection.fb2")
        file.writeText("""<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>Highlight selection fixture</book-title><lang>en</lang></title-info></description><body><section><title><p>Chapter one</p></title><p>Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu.</p><p>Another paragraph keeps source positions distinct and remains outside the first highlight.</p></section></body></FictionBook>""".replace("</description>","<document-info><id>${java.util.UUID.randomUUID()}</id></document-info></description>"))
        val book=core.importBook(file.absolutePath).single {it.title=="Highlight selection fixture"}
        var activity:MainActivity?=null;var model:ReaderModel?=null;var previous:ScreenState?=null
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        try {
            core.saveAiOptions(ai.copy(layoutEnabled=false));core.saveReadingOptions(reading.copy(translated=false))
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(8000) as MainActivity
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java]}
            await("model ready"){model!!.state.value.loaded&&!model!!.state.value.busy};previous=model!!.state.value
            instrumentation.runOnMainSync {model!!.interfaceSettings("light","en");model!!.open(book)}
            await("book opens"){model!!.state.value.frame!=null&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {model!!.focus(true)};await("focus opens"){model!!.state.value.focus&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {model!!.selectionMode("free")};await("free mode sets"){model!!.state.value.selectionMode=="free"&&!model!!.state.value.busy}
            SystemClock.sleep(500)
            val bounds=model!!.state.value.frame!!.info.regions.first {it.text.startsWith("Alpha bravo")}.bounds.first()
            val y=bounds.y+bounds.height/2
            fun select(left:Float,right:Float,highlighted:Boolean){
                instrumentation.runOnMainSync {model!!.select(bounds.x+left,y,bounds.x+right,y)}
                await("selection updates its source-based highlight state") {model!!.state.value.selected?.highlighted==highlighted&&label(if(highlighted)"Remove highlight"else"Highlight")!=null}
            }
            fun marked(count:Int){try{await("$count plain highlights") {model!!.state.value.selected==null&&!model!!.state.value.busy&&model!!.state.value.annotations.count {it.note==null}==count}}catch(error:AssertionError){throw AssertionError("$count plain highlights; annotations=${model!!.state.value.annotations}, selected=${model!!.state.value.selected}, error=${model!!.state.value.error}",error)}}
            select(2f,70f,false);click("Highlight");marked(1)
            select(2f,70f,true);assertNotNull(label("Remove"));shot("selection-remove-highlight.png");click("Remove highlight");marked(0)
            select(2f,70f,false);click("Highlight");marked(1)
            val original=model!!.state.value.annotations.single().id
            select(40f,150f,false);click("Highlight");marked(1)
            assertEquals(original,model!!.state.value.annotations.single().id)
            select(180f,230f,false);click("Highlight");marked(2)
            select(130f,210f,false);click("Highlight");marked(1)
            assertEquals(original,model!!.state.value.annotations.single().id)
            for(thought in listOf("First independent thought","Second independent thought")){
                select(2f,100f,false);click("Note")
                await("note input focuses without tapping") {node {it.isEditable&&it.isFocused}!=null&&label("Send note")!=null}
                await("note keyboard opens automatically"){keyboard()!=null}
                SystemClock.sleep(350)
                assertNull(label("Save"));assertNull(label("Add"));assertNull(label("Read together"))
                val input=node {it.isEditable&&it.isFocused}!!
                assertTrue(input.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT,Bundle().apply {putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,thought)}))
                await("quoted source remains a single line") {node {it.text?.toString()?.startsWith("Quote: ")==true}!=null}
                shot("note-composer-keyboard.png")
                val quoteRect=Rect().also {node {it.text?.toString()?.startsWith("Quote: ")==true}!!.getBoundsInScreen(it)}
                assertTrue(quoteRect.height()/context.resources.displayMetrics.scaledDensity<=24f)
                val keyboardRect=Rect().also {keyboard()!!.getBoundsInScreen(it)}
                assertTrue("Quote remains above the keyboard",quoteRect.bottom<=keyboardRect.top+2)
                assertTrue("Composer stays attached to the keyboard",keyboardRect.top-quoteRect.bottom<=24f*context.resources.displayMetrics.density)
                click("Send note")
                await("sent note closes its editor") {label("Send note")==null&&!model!!.state.value.busy&&model!!.state.value.annotations.any {it.note==thought}}
            }
            assertEquals(1,model!!.state.value.annotations.count {it.note==null});assertEquals(2,model!!.state.value.annotations.count {it.note!=null})
            select(2f,230f,true);click("Remove highlight");marked(0)
            assertEquals(2,model!!.state.value.annotations.size)
            assertTrue(model!!.state.value.annotations.any {it.note=="First independent thought"})
        }finally {
            instrumentation.runOnMainSync {model?.back()};await("book closes"){model?.state?.value?.active==null&&model?.state?.value?.busy!=true}
            core.saveAiOptions(ai);core.saveReadingOptions(reading)
            previous?.let {before->instrumentation.runOnMainSync {model!!.interfaceSettings(before.appTheme,before.language)};await("interface restores"){!model!!.state.value.busy}}
            instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor)
            val edit=prefs.edit();for(key in listOf("focus","selectionFocus","selectionClassic","selection")){when(val value=oldPrefs[key]){is Boolean->edit.putBoolean(key,value);is String->edit.putString(key,value);else->edit.remove(key)}};check(edit.commit())
            core.removeBook(book.id);file.delete()
        }
    }
}

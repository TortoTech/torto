package org.tortotech.torto

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
class ReadingSelectionTest {
    @Test fun languageScriptsIncludeIsoAliasesAndExplicitRomanization(){
        for(tag in listOf("en","eng","fr","deu","vi","tr","sr-Latn","zh-Latn"))assertTrue(tag,bookSupportsWordSelection(listOf(tag)))
        for(tag in listOf("zh-CN","zho","chi","ja","jpn","ko","kor","ru","rus","ar","ara","hi","hin","el"))assertFalse(tag,bookSupportsWordSelection(listOf(tag)))
        assertTrue(bookSupportsWordSelection(emptyList()));assertTrue(bookSupportsWordSelection(listOf("und","en")))
        assertEquals("paragraph",effectiveReadingSelection("word",true,false))
        assertEquals("free",effectiveReadingSelection("word",false,false))
        assertEquals("sentence",effectiveReadingSelection("sentence",true,false))
    }
    @Test fun focusDefaultsToParagraphsAndChoicesSurviveBookScriptsAndModeSwitches():Unit=runBlocking {
        val instrumentation=InstrumentationRegistry.getInstrumentation();val context=instrumentation.targetContext
        val prefs=context.getSharedPreferences("reading",0);val originalPrefs=prefs.all
        val interfacePrefs=context.getSharedPreferences("interface",0);val originalInterface=interfacePrefs.all
        val core=NativeRuntime.core(context);val ai=core.aiOptions()
        val files=listOf("en","zh").map {language->File(context.cacheDir,"selection-$language.fb2").apply {writeText("""<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>Selection $language</book-title><lang>$language</lang></title-info></description><body><section><title><p>Selection</p></title><p>Text selection follows the authored language. This paragraph supports sentence and paragraph selection.</p></section></body></FictionBook>""")}}
        val books=files.zip(listOf("en","zh")).map {(file,language)->core.importBook(file.absolutePath).single {it.title=="Selection $language"}}
        var activity:MainActivity?=null;var model:ReaderModel?=null
        fun await(message:String,condition:()->Boolean){val end=SystemClock.uptimeMillis()+12000;while(!condition()){assertTrue(message,SystemClock.uptimeMillis()<end);SystemClock.sleep(25)}}
        fun label(text:String):AccessibilityNodeInfo? {
            instrumentation.uiAutomation.clearCache()
            fun visit(n:AccessibilityNodeInfo?):AccessibilityNodeInfo? {if(n==null)return null;if(n.isVisibleToUser&&(n.text?.toString()==text||n.contentDescription?.toString()==text))return n;for(i in 0 until n.childCount)visit(n.getChild(i))?.let {return it};return null}
            return visit(instrumentation.uiAutomation.rootInActiveWindow)
        }
        fun click(text:String){await(text){label(text)!=null};var n=label(text);while(n!=null&&!n.isClickable)n=n.parent;assertTrue(n!!.performAction(AccessibilityNodeInfo.ACTION_CLICK));instrumentation.waitForIdleSync()}
        fun open(index:Int){instrumentation.runOnMainSync {model!!.open(books[index])};await("book opens"){model!!.state.value.active?.id==books[index].id&&model!!.state.value.frame!=null&&!model!!.state.value.busy}}
        try {
            check(prefs.edit().putBoolean("focus",true).remove("selectionFocus").remove("selectionClassic").remove("selection").commit());core.saveAiOptions(ai.copy(layoutEnabled=false))
            val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(8000) as MainActivity;instrumentation.removeMonitor(monitor)
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java]}
            await("model ready"){model!!.state.value.loaded&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {model!!.interfaceSettings("light","en")}
            open(0);assertTrue(model!!.state.value.focus);assertEquals("paragraph",model!!.state.value.selectionMode)
            instrumentation.runOnMainSync {model!!.selectionMode("word")};await("Latin word choice saves"){model!!.state.value.selectionMode=="word"&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {model!!.focus(false)};await("classic has its own default"){!model!!.state.value.focus&&model!!.state.value.selectionMode=="free"&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {model!!.selectionMode("sentence")};await("classic choice saves"){model!!.state.value.selectionMode=="sentence"&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {model!!.focus(true)};await("focus restores its Latin preference"){model!!.state.value.focus&&model!!.state.value.selectionMode=="word"&&!model!!.state.value.busy}
            open(1);assertFalse(model!!.state.value.wordSelectionAvailable);assertEquals("paragraph",model!!.state.value.selectionMode)
            assertEquals("word",prefs.getString("selectionFocus",null))
            instrumentation.runOnMainSync {model!!.toggleChrome()};click("Selection mode")
            await("non-Latin selection panel opens"){label("Paragraph")!=null&&label("Free")!=null};assertNull(label("Word"))
            click("Sentence")
            await("selection closes its panel and saves immediately"){label("Free")==null&&model!!.state.value.selectionMode=="sentence"&&!model!!.state.value.busy}
            assertEquals("sentence",prefs.getString("selectionFocus",null))
            instrumentation.runOnMainSync {model!!.selectionMode("word")} // hidden choice is also rejected by the model
            assertEquals("sentence",model!!.state.value.selectionMode)
            instrumentation.runOnMainSync {model!!.selectionMode("paragraph")}
            await("paragraph choice saves"){!model!!.state.value.busy}
            open(0);assertTrue(model!!.state.value.wordSelectionAvailable)
            instrumentation.runOnMainSync {model!!.selectionMode("word")}
            await("Latin choice applies"){model!!.state.value.selectionMode=="word"&&!model!!.state.value.busy}
            open(1);assertEquals("paragraph",model!!.state.value.selectionMode)
            open(0);assertTrue(model!!.state.value.wordSelectionAvailable);assertEquals("word",model!!.state.value.selectionMode)
        }finally {
            instrumentation.runOnMainSync {model?.back();activity?.finish()}
            await("reader closes"){model?.state?.value?.active==null&&model?.state?.value?.busy!=true}
            val edit=prefs.edit();for(key in listOf("focus","selection","selectionFocus","selectionClassic")){when(val value=originalPrefs[key]){is Boolean->edit.putBoolean(key,value);is String->edit.putString(key,value);else->edit.remove(key)}};check(edit.commit())
            val interfaceEdit=interfacePrefs.edit();for(key in listOf("theme","language")){(originalInterface[key] as? String)?.let {interfaceEdit.putString(key,it)}?:interfaceEdit.remove(key)};check(interfaceEdit.commit())
            core.saveAiOptions(ai);for(book in books)core.removeBook(book.id);files.forEach {it.delete()}
        }
    }
}

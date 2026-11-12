package org.tortotech.torto

import android.graphics.Bitmap
import android.graphics.Rect
import android.os.Bundle
import android.os.SystemClock
import android.view.MotionEvent
import android.view.accessibility.AccessibilityNodeInfo
import androidx.activity.compose.setContent
import androidx.compose.runtime.mutableStateOf
import androidx.lifecycle.ViewModelProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.runBlocking
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.BookSummary
import org.tortotech.torto.core.ChatMessage
import org.tortotech.torto.core.Provider
import java.io.File
import java.net.ServerSocket
import java.util.concurrent.atomic.AtomicReference

@RunWith(AndroidJUnit4::class)
class ReadingAssistantTest {
    private val instrumentation get()=InstrumentationRegistry.getInstrumentation()
    private fun await(message:String,condition:()->Boolean){val end=SystemClock.uptimeMillis()+12000;while(!condition()){if(SystemClock.uptimeMillis()>=end){screenshot("assistant-failure.png");fail(message)};SystemClock.sleep(30)}}
    private fun node(predicate:(AccessibilityNodeInfo)->Boolean):AccessibilityNodeInfo? {
        instrumentation.uiAutomation.clearCache()
        fun visit(n:AccessibilityNodeInfo?):AccessibilityNodeInfo? {if(n==null)return null;if(n.isVisibleToUser&&predicate(n))return n;for(i in 0 until n.childCount)visit(n.getChild(i))?.let {return it};return null}
        return visit(instrumentation.uiAutomation.rootInActiveWindow)
    }
    private fun label(text:String)=node {it.text?.toString()==text||it.contentDescription?.toString()==text}
    private fun click(text:String){
        fun button():AccessibilityNodeInfo? {var n=label(text);while(n!=null&&!n.isClickable)n=n.parent;return n}
        var target:AccessibilityNodeInfo?=null
        await("$text enabled"){button()?.takeIf {it.isEnabled}?.also {target=it}!=null}
        assertTrue(target!!.performAction(AccessibilityNodeInfo.ACTION_CLICK));instrumentation.waitForIdleSync()
    }
    private fun screenshot(name:String){SystemClock.sleep(200);instrumentation.uiAutomation.takeScreenshot()!!.let {image->try {File(instrumentation.targetContext.cacheDir,name).outputStream().use {image.compress(Bitmap.CompressFormat.PNG,100,it)}}finally {image.recycle()}}}
    private fun back(){instrumentation.sendKeyDownUpSync(android.view.KeyEvent.KEYCODE_BACK)}
    private fun launch():MainActivity {
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            return monitor.waitForActivityWithTimeout(8000) as MainActivity
        }finally {instrumentation.removeMonitor(monitor)}
    }
    @Test fun fullScreenHistoryRetainsDraftSourcesAndKeyboardLayout():Unit=runBlocking {
        val context=instrumentation.targetContext;val core=NativeRuntime.core(context)
        val ai=core.aiOptions();val before=core.books().map {it.id}.toSet()
        val file=File(context.cacheDir,"assistant-ui.fb2")
        file.writeText("""<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>Assistant visual fixture</book-title><lang>en</lang></title-info></description><body><section><title><p>Source chapter</p></title><p>This passage grounds the answer in the actual imported book. Sources must resolve to the original chapter.</p></section></body></FictionBook>""")
        val book=core.importBook(file.absolutePath).single {it.title=="Assistant visual fixture"}
        val history=File(context.filesDir,"core/chat/${book.id}.json")
        val server=ServerSocket(0,1,java.net.InetAddress.getByName("127.0.0.1"))
        val serverFailure=AtomicReference<Throwable?>()
        val worker=kotlin.concurrent.thread {
            try {server.accept().use {socket->
                socket.soTimeout=8000;val input=socket.getInputStream();val headers=StringBuilder()
                while(!headers.endsWith("\r\n\r\n")){val b=input.read();check(b>=0);headers.append(b.toChar())}
                val length=headers.lines().first {it.startsWith("Content-Length:",true)}.substringAfter(':').trim().toInt()
                var remaining=length;val buffer=ByteArray(4096);while(remaining>0){val count=input.read(buffer,0,minOf(buffer.size,remaining));check(count>0);remaining-=count}
                fun event(delta:JSONObject)="data: "+JSONObject().put("choices",JSONArray().put(JSONObject().put("index",0).put("delta",delta)))+"\n\n"
                val body=event(JSONObject().put("reasoning_content","Compare relevant passages."))+event(JSONObject().put("role","assistant").put("content","## Main idea\n\nA grounded answer with a [source](link://j/0)."))+"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
                val bytes=body.toByteArray();socket.getOutputStream().apply {write("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: ${bytes.size}\r\nConnection: close\r\n\r\n".toByteArray());write(bytes);flush()}
            }}catch(e:Throwable){if(!server.isClosed)serverFailure.set(e)}
        }
        var providerId:String?=null
        var activity:MainActivity?=null;var model:ReaderModel?=null;var original:ScreenState?=null
        try {
            core.saveProvider(Provider("","custom","Assistant UI fixture","http://127.0.0.1:${server.localPort}/v1",listOf("fixture"),false),"test-only",false)
            providerId=core.providers().single {it.name=="Assistant UI fixture"}.id
            core.saveAiOptions(ai.copy(layoutEnabled=false,chatProvider=providerId,chatModel="fixture"));activity=launch()
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java]}
            await("model ready"){model!!.state.value.loaded&&!model!!.state.value.busy};original=model!!.state.value
            instrumentation.runOnMainSync {model!!.interfaceSettings("light","en");model!!.configure(original!!.fontSize,original!!.unified,false);model!!.open(book)}
            await("book opens"){model!!.state.value.frame!=null&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {model!!.sendChat("Explain this passage")}
            await("local streamed answer completes"){model!!.state.value.messages.size==2&&!model!!.state.value.chatWorking}
            serverFailure.get()?.let {throw it}
            val answer=model!!.state.value.messages.last()
            val thoughtLabel="Reasoning (${answer.thinkingSeconds}s)"
            instrumentation.runOnMainSync {if(!model!!.state.value.chromeVisible)model!!.toggleChrome()}
            click("Reading assistant")
            await("full-screen assistant loads saved turns"){label("Collapse assistant")!=null&&label("Explain this passage")!=null&&!model!!.state.value.busy}
            await("assistant input focuses and opens its keyboard automatically"){node {it.isEditable&&it.isFocused}!=null&&label("Highlights")==null}
            screenshot("assistant-autofocus.png")
            back();await("keyboard can be hidden without closing the assistant"){label("Highlights")!=null&&label("Collapse assistant")!=null}
            val pane=node {it.paneTitle?.toString()=="Reading assistant"}!!
            val bounds=Rect().also(pane::getBoundsInScreen)
            assertEquals(0,bounds.top);assertEquals(activity!!.window.decorView.height,bounds.bottom)
            fun pullDown(y:Float){
                val width=activity!!.window.decorView.width.toFloat();val height=activity!!.window.decorView.height.toFloat();val start=SystemClock.uptimeMillis()
                fun event(action:Int,at:Float){MotionEvent.obtain(start,SystemClock.uptimeMillis(),action,width*.4f,at,0).also {instrumentation.sendPointerSync(it);it.recycle()}}
                event(MotionEvent.ACTION_DOWN,y)
                repeat(10){i->SystemClock.sleep(20);event(MotionEvent.ACTION_MOVE,y+height*.025f*(i+1))}
                event(MotionEvent.ACTION_UP,y+height*.25f)
                SystemClock.sleep(200)
                assertNotNull("Pulling the assistant never dismisses it",label("Collapse assistant"))
                assertEquals(bounds,Rect().also {node {it.paneTitle?.toString()=="Reading assistant"}!!.getBoundsInScreen(it)})
            }
            pullDown(Rect().also {label("Reading assistant")!!.getBoundsInScreen(it)}.exactCenterY())
            pullDown(activity!!.window.decorView.height*.22f)
            assertNull(label("Typography"));assertNull(label("Attach images"));assertNull(label("Copy"));assertNull(label("Share"))
            assertNull("Finished thoughts start collapsed",label("Compare relevant passages."))
            click(thoughtLabel);await("reasoning is retained in the reading session"){label("Compare relevant passages.")!=null}
            screenshot("assistant-history.png")
            click(thoughtLabel);click("Book sources · 1")
            await("real source preview loads"){model!!.state.value.chatSources[answer.id]?.size==1}
            val source=model!!.state.value.chatSources[answer.id]!!.single()
            assertTrue(source.excerpt.contains("grounds the answer"))
            assertFalse("The heading is not repeated in the source excerpt",source.excerpt.startsWith(source.title))
            await("inline links are numbered circles"){node {it.text?.toString()?.contains("①")==true}!=null}
            click("① ${source.title}")
            await("citation returns to the book"){label("Collapse assistant")==null&&model!!.state.value.frame?.info?.canGoBack==true&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {if(!model!!.state.value.chromeVisible)model!!.toggleChrome()}
            click("Reading assistant")
            await("reopening focuses the draft"){node {it.isEditable&&it.isFocused}!=null&&label("Highlights")==null}
            back()
            await("expanded sources restore on reopening"){label("① ${source.title}")!=null&&label("Highlights")!=null}
            val header=Rect().also {label("Collapse assistant")!!.getBoundsInScreen(it)}
            val input=node {it.isEditable}!!
            assertTrue(input.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT,Bundle().apply {putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,"A draft for later")}))
            val r=Rect().also(input::getBoundsInScreen);val now=SystemClock.uptimeMillis()
            for(action in listOf(MotionEvent.ACTION_DOWN,MotionEvent.ACTION_UP))MotionEvent.obtain(now,SystemClock.uptimeMillis(),action,r.exactCenterX(),r.exactCenterY(),0).also {instrumentation.sendPointerSync(it);it.recycle()}
            await("keyboard hides shortcuts and keeps the header fixed"){label("Highlights")==null&&node {it.isEditable&&it.isFocused}!=null}
            assertEquals(header,Rect().also {label("Collapse assistant")!!.getBoundsInScreen(it)})
            screenshot("assistant-keyboard.png")
            back();await("back dismisses the keyboard first"){label("Highlights")!=null&&label("Collapse assistant")!=null}
            click("Collapse assistant");await("assistant collapses"){label("Collapse assistant")==null}
            instrumentation.runOnMainSync {if(!model!!.state.value.chromeVisible)model!!.toggleChrome()};click("Reading assistant")
            await("draft survives closing and regains focus"){node {it.isEditable&&it.isFocused&&it.text?.toString()=="A draft for later"}!=null&&label("Highlights")==null}
            back();await("keyboard closes before checking answer menus"){label("Highlights")!=null}
            click("Assistant menu");click("Clear conversation");click("Cancel")
            assertEquals(2,model!!.state.value.messages.size)
            await("answer actions are visible after the confirmation closes"){label("Answer menu")!=null}
            val menuAnchor=Rect().also {label("Answer menu")!!.getBoundsInScreen(it)}
            click("Answer menu")
            await("answer menu opens above its own button"){
                node {it.paneTitle?.toString()=="More actions"}?.let {Rect().also(it::getBoundsInScreen).bottom<=menuAnchor.top}==true
            }
            screenshot("assistant-answer-menu.png")
            click("Delete")
            await("deletion confirmation replaces the popup"){label("Delete this question and answer?")!=null&&node {it.paneTitle?.toString()=="More actions"}==null}
            click("Delete")
            await("deleting one turn updates history"){model!!.state.value.messages.isEmpty()&&!model!!.state.value.busy}
            assertFalse("Conversation is never written to disk",history.exists())
            back();await("system back returns to the reader without closing the book"){label("Collapse assistant")==null&&model!!.state.value.active?.id==book.id}
        }finally {
            original?.let {saved->instrumentation.runOnMainSync {model!!.back()};await("book closes"){model!!.state.value.active==null&&!model!!.state.value.busy};instrumentation.runOnMainSync {model!!.configure(saved.fontSize,saved.unified,saved.dark);model!!.focus(saved.focus);model!!.interfaceSettings(saved.appTheme,saved.language)};await("settings restored"){!model!!.state.value.busy}}
            core.saveAiOptions(ai);providerId?.let {core.deleteProvider(it)};server.close();worker.join(1000);instrumentation.runOnMainSync {activity?.finish()}
            if(book.id !in before)core.removeBook(book.id);history.delete();file.delete()
        }
    }
    @Test fun streamingFollowsOnlyTheBottomAndLeavesManualHistoryInPlace(){
        val activity=launch();lateinit var model:ReaderModel
        instrumentation.runOnMainSync {model=ViewModelProvider(activity)[ReaderModel::class.java]}
        await("model loads"){model.state.value.loaded&&!model.state.value.busy}
        fun message(id:String,user:Boolean,text:String)=ChatMessage(id=id,user=user,content=text,images=emptyList(),progress=emptyList(),thinkingSeconds=null,stopped=false,citations=emptyList())
        val history=(0..19).flatMap {i->listOf(message("q$i",true,"Question $i"),message("a$i",false,"Answer $i. A grounded reading explanation with a little additional text."))}
        val screen=mutableStateOf(ScreenState(active=BookSummary(0.0,0uL,0uL,"assistant-scroll","Assistant scroll fixture",emptyList()),language="en",appTheme="light",messages=history,chatWorking=true,chatStreamId="stream",chatQuestion="Latest question",chatContent="Initial answer",chatReasoning="Compare the cited passages."))
        try {
            instrumentation.runOnMainSync {activity.setContent {TortoTheme(screen.value){ReadingAssistantDialog(screen.value,model,close={})}}}
            await("stream opens with focused input"){node {it.isEditable&&it.isFocused}!=null&&label("Highlights")==null}
            back();await("stream follows bottom"){label("Latest question")!=null&&label("Highlights")!=null}
            val height=activity.window.decorView.height.toFloat();val width=activity.window.decorView.width.toFloat();val now=SystemClock.uptimeMillis()
            fun event(action:Int,y:Float){MotionEvent.obtain(now,SystemClock.uptimeMillis(),action,width/2,y,0).also {instrumentation.sendPointerSync(it);it.recycle()}}
            event(MotionEvent.ACTION_DOWN,height*.3f)
            repeat(12){i->SystemClock.sleep(20);event(MotionEvent.ACTION_MOVE,height*(.3f+.035f*(i+1)))}
            event(MotionEvent.ACTION_UP,height*.72f)
            await("manual scrolling exposes back-to-bottom"){label("Back to bottom")!=null}
            SystemClock.sleep(250)
            val visible=node {it.text?.toString()?.startsWith("Question ")==true}!!.text.toString()
            repeat(8){i->instrumentation.runOnMainSync {screen.value=screen.value.copy(chatContent="Updated streamed answer $i. ".repeat(10))};SystemClock.sleep(50)}
            SystemClock.sleep(250);assertNotNull("Token updates must not steal the reading position",label(visible))
            screenshot("assistant-streaming-history.png")
            click("Back to bottom");await("explicit action resumes following"){label("Latest question")!=null&&label("Back to bottom")==null}
            instrumentation.runOnMainSync {screen.value=screen.value.copy(dark=true)}
            await("dark scheme settles"){
                instrumentation.uiAutomation.takeScreenshot()!!.let {image->try {android.graphics.Color.red(image.getPixel(0,image.height/2))<80}finally {image.recycle()}}
            }
            screenshot("assistant-dark.png")
            assertNotNull(label("Stop generating"));assertNull(label("Attach images"));assertNull(label("Copy"));assertNull(label("Share"))
        }finally {instrumentation.runOnMainSync {activity.finish()}}
    }
}

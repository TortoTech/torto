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
class ReadingTypographyTest {
    private val instrumentation get()=InstrumentationRegistry.getInstrumentation()
    private fun await(message:String,condition:()->Boolean){
        val until=SystemClock.uptimeMillis()+10000
        while(!condition()){
            if(SystemClock.uptimeMillis()>=until){
                screenshot("reading-typography-failure.png")
                val tree=StringBuilder()
                fun dump(n:AccessibilityNodeInfo?,depth:Int){
                    if(n==null)return
                    tree.appendLine("${" ".repeat(depth)}${n.text} / ${n.contentDescription} clickable=${n.isClickable} editable=${n.isEditable} visible=${n.isVisibleToUser}")
                    for(i in 0 until n.childCount)dump(n.getChild(i),depth+1)
                }
                dump(instrumentation.uiAutomation.rootInActiveWindow,0)
                File(instrumentation.targetContext.cacheDir,"reading-typography-failure.txt").writeText(tree.toString())
                fail(message)
            }
            SystemClock.sleep(25)
        }
    }
    private fun node(predicate:(AccessibilityNodeInfo)->Boolean):AccessibilityNodeInfo? {
        instrumentation.uiAutomation.clearCache()
        fun visit(n:AccessibilityNodeInfo?):AccessibilityNodeInfo? {
            if(n==null)return null
            if(n.isVisibleToUser&&predicate(n))return n
            for(i in 0 until n.childCount)visit(n.getChild(i))?.let {return it}
            return null
        }
        return visit(instrumentation.uiAutomation.rootInActiveWindow)
    }
    private fun label(text:String)=node {it.text?.toString()==text||it.contentDescription?.toString()==text}
    private fun click(text:String){
        fun action():AccessibilityNodeInfo? {var target=label(text);while(target!=null&&!target.isClickable)target=target.parent;return target}
        await("$text is actionable"){action()?.isEnabled==true}
        assertTrue(action()!!.performAction(AccessibilityNodeInfo.ACTION_CLICK))
        instrumentation.waitForIdleSync()
    }
    private fun clickNode(node:AccessibilityNodeInfo){
        val rect=Rect().also(node::getBoundsInScreen)
        val down=SystemClock.uptimeMillis()
        for(action in listOf(android.view.MotionEvent.ACTION_DOWN,android.view.MotionEvent.ACTION_UP)){
            android.view.MotionEvent.obtain(down,SystemClock.uptimeMillis(),action,rect.exactCenterX(),rect.exactCenterY(),0).also {instrumentation.sendPointerSync(it);it.recycle()}
        }
    }
    private fun back(){instrumentation.sendKeyDownUpSync(android.view.KeyEvent.KEYCODE_BACK)}
    private fun screenshot(name:String){
        SystemClock.sleep(250)
        val image=instrumentation.uiAutomation.takeScreenshot()!!
        try {File(instrumentation.targetContext.cacheDir,name).outputStream().use {image.compress(Bitmap.CompressFormat.PNG,100,it)}}finally {image.recycle()}
    }
    @Test fun compactTypographyKeepsItsFooterFontChoicesAndModePreferences():Unit=runBlocking {
        val context=instrumentation.targetContext
        val core=NativeRuntime.core(context)
        val ai=core.aiOptions();val before=core.books().map {it.id}.toSet()
        val file=File(context.cacheDir,"reading-typography.fb2")
        file.writeText("""<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>Typography controls</book-title><lang>en</lang></title-info></description><body><section><title><p>Typography controls</p></title><p>Reader settings preserve the current paragraph while changing fonts and reading modes. This paragraph remains visible above the compact bottom panel.</p><p>A second paragraph checks that the reading context stays open when the settings panel returns.</p></section></body></FictionBook>""")
        val book=core.importBook(file.absolutePath).single {it.title=="Typography controls"}
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null;var model:ReaderModel?=null;var original:ScreenState?=null
        try {
            core.saveAiOptions(ai.copy(layoutEnabled=false,layoutReasoning="none"))
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(8000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java]}
            await("shelf initializes"){model!!.state.value.loaded&&!model!!.state.value.busy};original=model!!.state.value
            instrumentation.runOnMainSync {model!!.interfaceSettings("light","en");model!!.open(book)}
            await("book opens"){model!!.state.value.active?.id==book.id&&model!!.state.value.frame!=null&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {model!!.configure(20f,false,false)}
            await("base typography applies"){!model!!.state.value.busy&&model!!.state.value.fontSize==20f&&!model!!.state.value.unified}
            instrumentation.runOnMainSync {model!!.focus(true)}
            await("focus applies"){model!!.state.value.focus&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {if(!model!!.state.value.chromeVisible)model!!.toggleChrome()}
            click("Typography")
            await("compact entries appear"){label("Font styles")!=null&&label("AI layout")!=null&&label("Focus mode")!=null}
            assertNull(label("Margins"));assertNull(label("Line spacing"));assertNull(label("CJK books"));assertNull(label("Unified layout"))
            screenshot("reading-typography-main.png")
            click("Increase font size")
            await("font size increases and saves"){!model!!.state.value.busy&&model!!.state.value.fontSize==22f}
            assertEquals(22f,context.getSharedPreferences("reading",0).getFloat("size",0f))
            click("Decrease font size")
            await("font size decreases"){!model!!.state.value.busy&&model!!.state.value.fontSize==20f}
            for((request,expected) in listOf(23f to 24f,16f to 16f,40f to 40f,20f to 20f)){
                await("font slider is adjustable"){node {it.rangeInfo!=null}!=null}
                val slider=node {it.rangeInfo!=null}!!
                assertEquals(16f,slider.rangeInfo!!.min);assertEquals(40f,slider.rangeInfo!!.max)
                assertTrue(slider.performAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_SET_PROGRESS.id,Bundle().apply {putFloat(AccessibilityNodeInfo.ACTION_ARGUMENT_PROGRESS_VALUE,request)}))
                await("font size snaps to $expected"){!model!!.state.value.busy&&model!!.state.value.fontSize==expected}
            }
            click("Font styles")
            await("font style settings open"){model!!.state.value.fontFamilies!=null&&label("Code font")!=null}
            assertNull(label("Font size"));assertNotNull(label("Typography"))
            screenshot("reading-font-style.png")
            click("Code font")
            await("font selector remains in the reading panel"){label("Search fonts")!=null&&label("Typography")!=null}
            val family=model!!.state.value.fontFamilies!!.monospace.first()
            node {it.isEditable}!!.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT,Bundle().apply {putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,family)})
            await("font search filters"){node {!it.isEditable&&it.text?.toString()==family}!=null}
            var option=node {!it.isEditable&&it.text?.toString()==family}!!
            while(!option.isClickable)option=option.parent!!
            assertTrue(option.performAction(AccessibilityNodeInfo.ACTION_CLICK))
            await("font choice returns one level"){label("Code font")!=null&&label("Search fonts")==null&&!model!!.state.value.busy&&model!!.state.value.reading?.monospaceFont==family}
            back();await("back returns to compact panel"){label("AI layout")!=null&&label("CJK books")==null}
            click("Focus mode");await("reading mode choices appear"){label("Reading mode")!=null&&label("Classic mode")!=null}
            assertNull(label("Unified layout"))
            click("Classic mode")
            await("classic mode exposes unified layout"){!model!!.state.value.focus&&!model!!.state.value.busy&&label("Unified layout")!=null}
            screenshot("reading-mode-classic.png")
            click("Unified layout")
            await("classic layout preference saves"){model!!.state.value.unified&&!model!!.state.value.busy}
            click("Focus mode")
            await("focus hides the classic option"){model!!.state.value.focus&&!model!!.state.value.busy&&label("Unified layout")==null}
            click("Classic mode")
            await("classic preference survives focus"){!model!!.state.value.focus&&model!!.state.value.unified&&!model!!.state.value.busy}
            click("Focus mode");await("focus restores"){model!!.state.value.focus&&!model!!.state.value.busy}
            back();await("compact panel returns"){label("Font styles")!=null&&label("Reading mode")==null}
            click("AI layout")
            await("AI layout details appear"){label("AI layout settings")!=null&&label("Reasoning effort")!=null&&!model!!.state.value.busy}
            assertNull(label("Save"));assertNull(label("Manage providers"))
            click("Reasoning effort");click("Low")
            await("reasoning choice returns to AI details"){label("AI layout settings")!=null&&label("Reasoning effort")!=null}
            back()
            await("AI settings save when returning"){label("Font styles")!=null&&model!!.state.value.aiOptions?.layoutReasoning=="low"&&!model!!.state.value.busy}
            click("AI layout");await("AI details reopen"){label("AI layout settings")!=null&&!model!!.state.value.busy}
            click("Reasoning effort");click("None")
            await("reasoning reset returns to AI details"){label("AI layout settings")!=null&&label("Reasoning effort")!=null}
            back()
            await("AI settings restore"){label("Font styles")!=null&&model!!.state.value.aiOptions?.layoutReasoning=="none"&&!model!!.state.value.busy}
            back();await("panel dismisses without leaving the book"){label("Font styles")==null&&model!!.state.value.active?.id==book.id}
            click("Theme")
            await("theme panel opens"){label("Brightness")!=null&&label("Color")!=null&&label("Light")!=null&&label("Dark")!=null}
            val themeBounds=Rect().also {label("Theme")!!.getBoundsInScreen(it)}
            val typeBounds=Rect().also {label("Typography")!!.getBoundsInScreen(it)}
            assertTrue("Typography is the last footer entry",typeBounds.left>themeBounds.right)
            val brightnessSlider=node {it.rangeInfo!=null}!!
            assertTrue(brightnessSlider.performAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_SET_PROGRESS.id,Bundle().apply {putFloat(AccessibilityNodeInfo.ACTION_ARGUMENT_PROGRESS_VALUE,.65f)}))
            await("brightness applies to the window"){model!!.state.value.readingBrightness==.65f&&activity!!.window.attributes.screenBrightness==.65f}
            screenshot("reading-theme-light.png")
            click("Dark")
            await("dark paper applies"){model!!.state.value.dark&&!model!!.state.value.busy}
            screenshot("reading-theme-dark.png")
            click("Light")
            await("light paper applies"){!model!!.state.value.dark&&!model!!.state.value.busy}
            back();await("theme closes"){label("Brightness")==null}
        }finally {
            original?.let {saved->
                instrumentation.runOnMainSync {model!!.back()}
                await("reader closes"){model!!.state.value.active==null&&!model!!.state.value.busy}
                await("leaving reading restores system brightness"){activity!!.window.attributes.screenBrightness<0f}
                instrumentation.runOnMainSync {model!!.setReadingBrightness(saved.readingBrightness)}
                saved.reading?.let {value->instrumentation.runOnMainSync {model!!.saveReadingOptions(value)};await("fonts restore"){!model!!.state.value.busy&&model!!.state.value.reading==value}}
                instrumentation.runOnMainSync {model!!.configure(saved.fontSize,saved.unified,saved.dark);model!!.focus(saved.focus);model!!.interfaceSettings(saved.appTheme,saved.language)}
                await("settings restore"){!model!!.state.value.busy&&model!!.state.value.focus==saved.focus&&model!!.state.value.fontSize==saved.fontSize&&model!!.state.value.unified==saved.unified&&model!!.state.value.appTheme==saved.appTheme&&model!!.state.value.language==saved.language}
            }
            core.saveAiOptions(ai)
            instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor)
            if(book.id !in before)core.removeBook(book.id);file.delete()
        }
    }
}

package org.tortotech.torto

import android.graphics.Bitmap
import android.os.Bundle
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
class SettingsChoicePagesTest {
    private val instrumentation get()=InstrumentationRegistry.getInstrumentation()
    private fun await(message:String,condition:()->Boolean){val end=android.os.SystemClock.uptimeMillis()+8000;while(!condition()){assertTrue(message,android.os.SystemClock.uptimeMillis()<end);android.os.SystemClock.sleep(25)}}
    private fun node(predicate:(AccessibilityNodeInfo)->Boolean):AccessibilityNodeInfo?{
        if(android.os.Build.VERSION.SDK_INT>=33)instrumentation.uiAutomation.clearCache()
        val queue=java.util.ArrayDeque<AccessibilityNodeInfo>()
        instrumentation.uiAutomation.rootInActiveWindow?.let(queue::add)
        while(queue.isNotEmpty()){
            val n=queue.removeFirst();if(n.isVisibleToUser&&predicate(n))return n
            for(i in 0 until n.childCount)n.getChild(i)?.let(queue::add)
        }
        return null
    }
    private fun label(text:String)=node {it.text?.toString()==text||it.contentDescription?.toString()==text}
    private fun clickable(n:AccessibilityNodeInfo?):AccessibilityNodeInfo?{var x=n;while(x!=null&&!x.isClickable)x=x.parent;return x?.takeIf {it.isEnabled}}
    private fun click(text:String){await("$text is actionable"){clickable(label(text))!=null};assertTrue(clickable(label(text))!!.performAction(AccessibilityNodeInfo.ACTION_CLICK))}
    private fun tap(text:String){
        val bounds=android.graphics.Rect().also {label(text)!!.getBoundsInScreen(it)}
        val now=android.os.SystemClock.uptimeMillis()
        for((action,time) in listOf(0 to now,1 to now+60))android.view.MotionEvent.obtain(now,time,action,bounds.exactCenterX(),bounds.exactCenterY(),0).let {instrumentation.sendPointerSync(it);it.recycle()}
    }
    private fun field(text:String):AccessibilityNodeInfo?{var x=label(text);while(x!=null&&!x.isEditable)x=x.parent;return x}
    private fun fill(title:String,text:String){await("$title is editable"){field(title)!=null};assertTrue(field(title)!!.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT,Bundle().apply {putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,text)}))}
    private fun screenshot(name:String){instrumentation.waitForIdleSync();android.os.SystemClock.sleep(180);instrumentation.uiAutomation.takeScreenshot()?.let {bitmap->File(instrumentation.targetContext.cacheDir,name).outputStream().use {bitmap.compress(Bitmap.CompressFormat.PNG,100,it)};bitmap.recycle()}}
    @Test fun providerAndCloudDraftsSurviveChoicePagesAndFontSearchRestoresItsParent():Unit=runBlocking {
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null;var model:ReaderModel?=null;var previous:ScreenState?=null
        val core=NativeRuntime.core(instrumentation.targetContext)
        val originalCloud=core.cloudOptions();val originalReading=core.readingOptions()
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java]}
            await("model loads"){model!!.state.value.loaded&&!model!!.state.value.busy};previous=model!!.state.value
            instrumentation.runOnMainSync {model!!.interfaceSettings("light","en")}
            await("English applies"){model!!.state.value.language=="en"&&!model!!.state.value.busy}
            click("Me");click("Settings");click("AI providers");click("Add provider")
            await("editor appears"){field("Name")!=null}
            fill("Name","Uncommitted choice fixture");fill("Base URL","http://127.0.0.1:1/v1");fill("API Key","fixture-key-only")
            click("Provider type")
            await("provider type opens as a page"){node {it.paneTitle?.toString()=="Provider type"}!=null}
            assertNull("Editor inputs are hidden from accessibility",label("Name"));assertNull(label("Save"))
            screenshot("choice-provider-type.png")
            tap("Custom")
            await("the exact draft returns"){field("Name")?.text?.toString()=="Uncommitted choice fixture"&&field("Base URL")?.text?.toString()=="http://127.0.0.1:1/v1"}
            assertTrue("API key draft survives choosing a type",field("API Key")!!.text?.isNotEmpty()==true)
            assertTrue(core.providers().none {it.name=="Uncommitted choice fixture"})
            click("Cancel");await("provider list returns"){label("Add provider")!=null};click("Back")
            await("settings returns"){label("Cloud sync")!=null};click("Cloud sync")
            await("cloud draft loads"){field("Device name")!=null&&!model!!.state.value.busy}
            for (obsolete in listOf("Foreground sync","Background sync","Account settings","Sync policy","Sync status")) {
                assertNull("Cloud settings have no obsolete policy control or outer heading: $obsolete",label(obsolete))
            }
            val passwordTitle=if(originalCloud.passwordSaved)"Password (saved; leave empty to keep)"else"Password"
            fill(passwordTitle,"fixture-password-only");fill("Device name","Uncommitted device fixture")
            click("Cloud provider")
            await("cloud provider is a page"){node {it.paneTitle?.toString()=="Cloud provider"}!=null}
            val preset=model!!.state.value.cloudPresets.first {it.id!=originalCloud.provider}
            click(preset.name)
            await("device draft returns after changing provider"){field("Device name")?.text?.toString()=="Uncommitted device fixture"}
            assertTrue("Password draft survives choosing a provider",field(passwordTitle)!!.text?.isNotEmpty()==true)
            assertEquals("Choosing does not save the whole cloud form",originalCloud,core.cloudOptions())
            screenshot("choice-cloud-draft.png")
            click("Back");await("settings returns from cloud"){label("Typesetting")!=null}
            click("Typesetting");await("font controls load"){label("Latin font")!=null&&!model!!.state.value.busy}
            click("Latin font")
            await("font choice has search"){node {it.paneTitle?.toString()=="Latin font"}!=null&&node {it.isEditable}!=null}
            val search=node {it.isEditable}!!
            assertTrue(search.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT,Bundle().apply {putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,"no-such-font-fixture")}))
            await("empty results are explicit"){label("No matching results")!=null}
            screenshot("choice-font-empty-search.png")
            click("Clear search")
            await("clearing search restores the list near the current font"){label("No matching results")==null&&node {it.isCheckable}!=null&&node {it.isEditable}?.text?.isEmpty()==true}
            screenshot("choice-font-list.png")
            click("Back")
            await("font page returns to its parent"){label("General")!=null&&label("Latin font")!=null}
            assertEquals(originalReading,core.readingOptions())
            click("Latin font");await("font page reopens"){node {it.paneTitle?.toString()=="Latin font"}!=null}
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("input keyevent 4")).use {it.readBytes()}
            await("system Back only closes the child"){label("General")!=null&&label("Latin font")!=null}
            assertNull(model!!.state.value.error)
        }catch(error:Throwable){
            screenshot("choice-pages-failure.png")
            val nodes=mutableListOf<String>()
            node {nodes.add("${it.className} text=${it.text} clickable=${it.isClickable} checkable=${it.isCheckable} enabled=${it.isEnabled}");false}
            File(instrumentation.targetContext.cacheDir,"choice-pages-failure.txt").writeText(nodes.joinToString("\n"))
            throw error
        }finally {
            previous?.let {saved->instrumentation.runOnMainSync {model!!.interfaceSettings(saved.appTheme,saved.language)};await("interface restores"){!model!!.state.value.busy}}
            instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor)
        }
    }
}

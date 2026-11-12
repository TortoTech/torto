package org.tortotech.torto

import android.os.Bundle
import android.graphics.Bitmap
import android.view.accessibility.AccessibilityNodeInfo
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.lifecycle.ViewModelProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.Provider
import java.io.File

@RunWith(AndroidJUnit4::class)
class AiSettingsAutosaveTest {
    private val instrumentation get()=InstrumentationRegistry.getInstrumentation()
    private fun await(message:String,condition:()->Boolean){
        val deadline=android.os.SystemClock.uptimeMillis()+6000
        while(!condition()){assertTrue(message,android.os.SystemClock.uptimeMillis()<deadline);android.os.SystemClock.sleep(25)}
    }
    private fun node(predicate:(AccessibilityNodeInfo)->Boolean):AccessibilityNodeInfo?{
        if(android.os.Build.VERSION.SDK_INT>=33)instrumentation.uiAutomation.clearCache()
        val queue=java.util.ArrayDeque<AccessibilityNodeInfo>()
        instrumentation.uiAutomation.rootInActiveWindow?.let(queue::add)
        while(queue.isNotEmpty()){
            val current=queue.removeFirst()
            if(current.isVisibleToUser&&predicate(current))return current
            for(index in 0 until current.childCount)current.getChild(index)?.let(queue::add)
        }
        return null
    }
    private fun label(value:String)=node {it.text?.toString()==value||it.contentDescription?.toString()==value}
    private fun click(item:AccessibilityNodeInfo?){
        var target=item;while(target!=null&&!target.isClickable)target=target.parent
        assertNotNull(target);assertTrue(target!!.performAction(AccessibilityNodeInfo.ACTION_CLICK))
    }
    private fun clickLabel(value:String){
        var target:AccessibilityNodeInfo?=null
        await("$value is actionable"){
            target=label(value)
            while(target!=null&&!target!!.isClickable)target=target!!.parent
            target?.isEnabled==true
        }
        click(target)
    }
    private fun screenshot(name:String){
        instrumentation.waitForIdleSync()
        android.os.SystemClock.sleep(120)
        instrumentation.uiAutomation.takeScreenshot()?.let {bitmap->
            File(instrumentation.targetContext.cacheDir,name).outputStream().use {bitmap.compress(Bitmap.CompressFormat.PNG,100,it)}
            bitmap.recycle()
        }
    }
    @Test fun rolesStageEditsAndSaveOnlyTheirFieldsOnReturn():Unit=runBlocking {
        val core=NativeRuntime.core(instrumentation.targetContext)
        val original=core.aiOptions()
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null;var model:ReaderModel?=null;var previous:ScreenState?=null;var providerId:String?=null
        val section=mutableStateOf("layout");val show=mutableStateOf(true)
        try {
            core.saveProvider(Provider("","custom","Autosave fixture","http://127.0.0.1:1/v1",listOf("model-a","model-b"),false),null,false)
            val provider=core.providers().single {it.name=="Autosave fixture"};providerId=provider.id
            core.saveAiOptions(original.copy(layoutEnabled=false,layoutProvider=provider.id,layoutModel="model-a",layoutReasoning="none",translationProvider=provider.id,translationModel="model-a",chatProvider=provider.id,chatModel="model-a",chatReasoning="none",targetLanguage="system",showOriginal=false,translateToc=false,expertTranslation=false))
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java]}
            await("model loads"){model!!.state.value.loaded&&!model!!.state.value.busy}
            previous=model!!.state.value
            instrumentation.runOnMainSync {model!!.interfaceSettings("light","en");model!!.loadServices()}
            await("English interface and provider models load"){model!!.state.value.language=="en"&&!model!!.state.value.busy&&model!!.state.value.providers.any {it.id==provider.id&&"model-b" in it.models}}
            instrumentation.runOnMainSync {activity!!.setContent {
                val state by model!!.state.collectAsState()
                TortoTheme(state){ChoicePageHost {Surface(Modifier.fillMaxSize()){if(show.value)AiRoleSettings(state,model!!,section.value)}}}
            }}
            await("layout controls render"){label("Layout model")!=null}
            screenshot("settings-layout-grouped.png")
            assertNull("Layout changes have no save button",label("Save"))
            assertNull(label("Manage providers and models"))
            click(node {it.isClickable&&it.isCheckable})
            clickLabel("Reasoning effort");clickLabel("Low")
            clickLabel("Layout model")
            await("model selection is a searchable page"){node {it.paneTitle?.toString()=="Layout model"}!=null&&node {it.isEditable}!=null}
            assertNull("The parent cannot be focused while choosing",label("AI layout"))
            assertEquals("Opening a child page does not commit its parent","model-a",core.aiOptions().layoutModel)
            assertTrue(node {it.isEditable}!!.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT,Bundle().apply {putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,"model-b")}))
            await("models filter by the search query"){label("model-b")!=null&&label("model-a")==null}
            screenshot("settings-model-choice-page.png")
            val result=node {it.text?.toString()=="model-b"&&!it.isEditable&&it.parent?.isCheckable==true}
            assertNotNull("The matching option is distinct from the search input",result)
            click(result)
            assertFalse("Editing does not apply layout before returning",core.aiOptions().layoutEnabled)
            assertEquals("none",core.aiOptions().layoutReasoning)
            assertEquals("model-a",core.aiOptions().layoutModel)
            instrumentation.runOnMainSync {section.value="translation"}
            await("returning commits layout and opens translation"){label("Translation model")!=null&&model!!.state.value.aiOptions!!.let {it.layoutEnabled&&it.layoutModel=="model-b"&&it.layoutReasoning=="low"}&&!model!!.state.value.busy}
            screenshot("settings-translation-grouped.png")
            assertNull("Translation changes have no save button",label("Save"))
            assertNull(label("Manage providers and models"))
            clickLabel("Target language");clickLabel("Custom")
            await("custom language field appears after the choice dialog closes"){label("Custom language")!=null&&node {it.className?.toString()=="android.widget.EditText"}!=null}
            assertEquals("An incomplete custom language never replaces the saved target","system",core.aiOptions().targetLanguage)
            val input=node {it.className?.toString()=="android.widget.EditText"}!!
            assertTrue(input.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT,Bundle().apply {putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,"Deutsch")}))
            android.os.SystemClock.sleep(350)
            assertEquals("Typing remains a draft until returning","system",core.aiOptions().targetLanguage)
            instrumentation.runOnMainSync {model!!.updateAiOptions {it.copy(showOriginal=true,expertTranslation=true)}}
            await("independent edits finish"){!model!!.state.value.busy&&model!!.state.value.aiOptions!!.expertTranslation}
            val latestInput=node {it.className?.toString()=="android.widget.EditText"}!!
            assertTrue(latestInput.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT,Bundle().apply {putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,"Français")}))
            // Return before another composition; the final keystroke must survive.
            instrumentation.runOnMainSync {show.value=false}
            await("returning saves the final language"){model!!.state.value.aiOptions!!.targetLanguage=="Français"&&!model!!.state.value.busy}
            instrumentation.runOnMainSync {section.value="assistant";show.value=true}
            await("assistant settings render"){label("Chat model")!=null}
            screenshot("settings-assistant-grouped.png")
            assertNull("Assistant has no save button",label("Save"));assertNull(label("Manage providers and models"))
            assertNull(label("Tool call steps"));assertNull(label("History turns"))
            clickLabel("Chat model");clickLabel("model-b")
            clickLabel("Reasoning effort");clickLabel("Low")
            assertEquals("Assistant changes remain staged","model-a",core.aiOptions().chatModel)
            instrumentation.runOnMainSync {show.value=false}
            await("returning saves assistant settings"){model!!.state.value.aiOptions!!.chatModel=="model-b"&&model!!.state.value.aiOptions!!.chatReasoning=="low"&&!model!!.state.value.busy}
            val saved=core.aiOptions()
            assertEquals("Français",saved.targetLanguage);assertEquals("model-b",saved.layoutModel);assertEquals("low",saved.layoutReasoning)
            assertTrue("A return must not overwrite newer untouched fields",saved.showOriginal)
            assertTrue(saved.expertTranslation)
            assertNull(model!!.state.value.error)
        }finally {
            instrumentation.runOnMainSync {show.value=false}
            await("role contents dispose"){label("Layout model")==null&&label("Translation model")==null&&label("Chat model")==null}
            if(model!=null){
                instrumentation.runOnMainSync {model!!.saveAiOptions(original);previous?.let {model!!.interfaceSettings(it.appTheme,it.language)}}
                await("original settings restore"){!model!!.state.value.busy&&model!!.state.value.aiOptions==original&&(previous==null||model!!.state.value.appTheme==previous!!.appTheme&&model!!.state.value.language==previous!!.language)}
            }else core.saveAiOptions(original)
            providerId?.let {core.deleteProvider(it)}
            activity?.let {instrumentation.runOnMainSync {it.finish()}}
            instrumentation.removeMonitor(monitor)
        }
    }
}

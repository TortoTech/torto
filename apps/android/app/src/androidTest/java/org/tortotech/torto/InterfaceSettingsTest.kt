package org.tortotech.torto

import android.graphics.Bitmap
import android.view.accessibility.AccessibilityNodeInfo
import androidx.lifecycle.ViewModelProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

@RunWith(AndroidJUnit4::class)
class InterfaceSettingsTest {
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
    private fun label(vararg values:String)=node {it.text?.toString() in values||it.contentDescription?.toString() in values}
    private fun click(item:AccessibilityNodeInfo?){
        var target=item
        while(target!=null&&!target.isClickable)target=target.parent
        assertNotNull("The displayed control must be clickable",target)
        assertTrue(target!!.performAction(AccessibilityNodeInfo.ACTION_CLICK))
    }
    private fun fill(value:String,vararg labels:String){
        var target=label(*labels)
        while(target!=null&&!target.isEditable)target=target.parent
        assertNotNull("The field must be editable",target)
        assertTrue(target!!.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT,android.os.Bundle().apply {putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,value)}))
    }
    private fun toggle():AccessibilityNodeInfo?{
        var target=label("跟随系统","Follow system")
        while(target!=null&&!target.isClickable)target=target.parent
        return target?.takeIf {it.isCheckable}
    }
    private fun darkBackgroundVisible():Boolean=instrumentation.uiAutomation.takeScreenshot()?.let {bitmap->
        val dark=android.graphics.Color.luminance(bitmap.getPixel(bitmap.width/2,bitmap.height*4/5))<.5f
        bitmap.recycle();dark
    } ?: false
    private fun screenshot(name:String){
        instrumentation.waitForIdleSync()
        android.os.SystemClock.sleep(120)
        instrumentation.uiAutomation.takeScreenshot()?.let {bitmap->
            File(instrumentation.targetContext.cacheDir,name).outputStream().use {bitmap.compress(Bitmap.CompressFormat.PNG,100,it)}
            bitmap.recycle()
        }
    }

    @Test fun darkModeAndLanguagePagesSaveChoicesFromSettingsHome(){
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null;var model:ReaderModel?=null;var original:ScreenState?=null;var fixtureProvider:String?=null
        val preferences=instrumentation.targetContext.getSharedPreferences("interface",0)
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java]}
            await("settings load"){model!!.state.value.loaded&&!model!!.state.value.busy}
            original=model!!.state.value
            instrumentation.runOnMainSync {model!!.interfaceSettings("light","zh-CN")}
            await("Chinese light interface applies"){model!!.state.value.language=="zh-CN"&&model!!.state.value.appTheme=="light"&&!model!!.state.value.busy}
            await("profile tab is visible"){label("我")!=null};click(label("我"))
            await("settings entry is visible"){label("设置")!=null};click(label("设置"))
            await("settings home shows direct interface choices"){label("深色模式")!=null&&label("界面语言")!=null&&label("排版")!=null&&label("未开启")!=null&&label("中文")!=null}
            assertNull("No intermediate system page",label("系统"))
            val darkBounds=android.graphics.Rect().also(label("深色模式")!!::getBoundsInScreen)
            val languageBounds=android.graphics.Rect().also(label("界面语言")!!::getBoundsInScreen)
            val typesettingBounds=android.graphics.Rect().also(label("排版")!!::getBoundsInScreen)
            assertTrue("Both interface choices precede typesetting",darkBounds.bottom<languageBounds.top&&languageBounds.bottom<typesettingBounds.top)
            screenshot("interface-settings-grouped.png")
            click(label("深色模式"))
            await("dark mode opens its own choice page"){toggle()!=null&&label("浅色")!=null}
            screenshot("interface-dark-mode-light.png")
            click(label("深色"))
            await("dark mode applies immediately"){model!!.state.value.appTheme=="dark"&&!model!!.state.value.busy&&darkBackgroundVisible()}
            assertEquals("dark",preferences.getString("theme",null))
            screenshot("interface-dark-mode-dark.png")
            click(label("返回"))
            await("system shows dark mode on"){label("已开启")!=null}
            click(label("深色模式"))
            await("follow-system switch is actionable"){toggle()!=null};click(toggle())
            await("system mode replaces the manual choice"){model!!.state.value.appTheme=="system"&&!model!!.state.value.busy&&label("手动选择")==null}
            assertEquals("system",preferences.getString("theme",null))
            assertNull(label("手动选择"))
            click(label("返回"))
            await("settings summary shows follow system"){label("跟随系统")!=null&&label("界面语言")!=null&&label("排版")!=null}
            click(label("界面语言"))
            await("language opens its own choice page"){label("English")!=null&&toggle()!=null}
            click(label("English"))
            await("English applies without leaving the page"){model!!.state.value.language=="en"&&!model!!.state.value.busy&&label("Interface language")!=null}
            assertEquals("en",preferences.getString("language",null))
            screenshot("interface-language-english.png")
            click(label("Back"))
            await("English summary stays visible on returning"){label("English")!=null&&label("Dark mode")!=null}
            click(label("Interface language"))
            await("language follow-system switch is actionable"){toggle()!=null};click(toggle())
            await("language follows system"){model!!.state.value.language=="system"&&!model!!.state.value.busy&&label("Manual selection","手动选择")==null}
            assertEquals("system",preferences.getString("language",null))
            assertNull(label("Manual selection","手动选择"))
            click(label("Back","返回"))
            await("settings home remains reachable after the language change"){label("Dark mode","深色模式")!=null}
            await("provider settings entry is visible"){label("AI providers","AI 提供商")!=null}
            click(label("AI providers","AI 提供商"))
            await("provider creation is available in the header"){label("Add provider","添加提供商")!=null}
            assertNull("The list no longer contains an add text button",node {it.text?.toString() in setOf("Add provider","添加提供商")})
            screenshot("interface-providers-header.png")
            click(label("Add provider","添加提供商"))
            await("the header action opens the provider editor"){label("Name","名称")!=null&&label("Base URL")!=null}
            screenshot("interface-provider-add-page.png")
            assertNotNull(label("Add AI provider","添加 AI 提供商"))
            assertNull(label("Close","关闭"))
            click(label("Back","返回"))
            await("closing discards the new provider draft"){label("Name","名称")==null}
            click(label("Back","返回"))
            await("settings returns after dismissing the editor"){label("Settings","设置")!=null&&label("Dark mode","深色模式")!=null}
            click(label("AI providers","AI 提供商"))
            await("reopening providers has no pending add request"){label("Add provider","添加提供商")!=null&&label("Name","名称")==null}
            click(label("Add provider","添加提供商"))
            await("add page opens again"){label("Name","名称")!=null}
            fill("Interface provider page fixture","Name","名称")
            fill("http://127.0.0.1:1/v1","Base URL")
            await("valid provider enables saving"){
                var target=label("Save","保存")
                while(target!=null&&!target.isClickable)target=target.parent
                target?.isEnabled==true
            }
            click(label("Save","保存"))
            await("saving returns to the provider list"){!model!!.state.value.busy&&label("Add provider","添加提供商")!=null&&label("Interface provider page fixture")!=null}
            fixtureProvider=model!!.state.value.providers.single {it.name=="Interface provider page fixture"}.id
            click(label("Interface provider page fixture"))
            await("editing also opens a page"){label("Edit AI provider","编辑 AI 提供商")!=null&&label("Base URL")!=null}
            click(label("Cancel","取消"))
            await("cancel returns to the provider list"){label("Add provider","添加提供商")!=null&&label("Name","名称")==null}
        }catch(error:Throwable){
            screenshot("interface-settings-failure.png")
            val nodes=mutableListOf<String>()
            node {nodes.add("${it.className} text=${it.text} description=${it.contentDescription} clickable=${it.isClickable} checkable=${it.isCheckable}");false}
            File(instrumentation.targetContext.cacheDir,"interface-settings-failure.txt").writeText(nodes.joinToString("\n"))
            throw error
        }finally {
            fixtureProvider?.let {id->instrumentation.runOnMainSync {model!!.deleteProvider(id)};await("provider fixture is removed"){!model!!.state.value.busy&&model!!.state.value.providers.none {it.id==id}}}
            original?.let {saved->instrumentation.runOnMainSync {model!!.interfaceSettings(saved.appTheme,saved.language)};await("original settings restore"){!model!!.state.value.busy&&model!!.state.value.appTheme==saved.appTheme&&model!!.state.value.language==saved.language}}
            activity?.let {instrumentation.runOnMainSync {it.finish()}}
            instrumentation.removeMonitor(monitor)
        }
    }
}

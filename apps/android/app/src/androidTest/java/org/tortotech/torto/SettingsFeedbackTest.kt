package org.tortotech.torto

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
class SettingsFeedbackTest {
    private val instrumentation get()=InstrumentationRegistry.getInstrumentation()
    private fun await(message:String,condition:()->Boolean){val until=android.os.SystemClock.uptimeMillis()+16000;while(!condition()){if(android.os.SystemClock.uptimeMillis()>=until){shot("settings-feedback-failure.png");fail(message)};android.os.SystemClock.sleep(40)}}
    private fun node(predicate:(AccessibilityNodeInfo)->Boolean):AccessibilityNodeInfo?{
        if(android.os.Build.VERSION.SDK_INT>=33)instrumentation.uiAutomation.clearCache()
        fun find(n:AccessibilityNodeInfo?):AccessibilityNodeInfo? {if(n==null)return null;if(n.isVisibleToUser&&predicate(n))return n;return (0 until n.childCount).firstNotNullOfOrNull {find(n.getChild(it))}}
        return find(instrumentation.uiAutomation.rootInActiveWindow)
    }
    private fun label(value:String)=node {it.text?.toString()==value||it.contentDescription?.toString()==value}
    private fun clickable(start:AccessibilityNodeInfo?):AccessibilityNodeInfo? {var node=start;while(node!=null&&!node.isClickable)node=node.parent;return node}
    private fun click(value:String){
        fun target()=node {(it.text?.toString()==value||it.contentDescription?.toString()==value)&&clickable(it)!=null}
        await("clickable action: $value"){target()!=null}
        assertTrue(clickable(target())!!.performAction(AccessibilityNodeInfo.ACTION_CLICK))
    }
    private fun shot(name:String){instrumentation.uiAutomation.takeScreenshot()?.let {bitmap->File(instrumentation.targetContext.cacheDir,name).outputStream().use {bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG,100,it)};bitmap.recycle()}}
    @Test fun simplifiedSettingsAndFeedbackRestoreDraftAndReadingPosition():Unit=runBlocking {
        val context=instrumentation.targetContext
        val directory=File(context.filesDir,"core/feedback").apply {mkdirs()}
        val names=listOf("editor.json","draft.json")
        val backup=names.associateWith {name->File(directory,name).takeIf {it.exists()}?.readBytes()}
        names.forEach {File(directory,it).delete()}
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null;var reader:ReaderModel?=null;var feedback:FeedbackModel?=null;var previous:ScreenState?=null
        val core=NativeRuntime.core(context);val originalAi=core.aiOptions();val originalReading=core.readingOptions();var imported:String?=null
        try {
            core.saveAiOptions(originalAi.copy(layoutEnabled=false));core.saveReadingOptions(originalReading.copy(translated=false))
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {reader=ViewModelProvider(activity!!)[ReaderModel::class.java];feedback=ViewModelProvider(activity!!)[FeedbackModel::class.java]}
            await("settings load"){reader!!.state.value.loaded&&!reader!!.state.value.busy};previous=reader!!.state.value
            instrumentation.runOnMainSync {reader!!.interfaceSettings("light","zh-CN")}
            await("language applies"){reader!!.state.value.language=="zh-CN"&&!reader!!.state.value.busy}
            click("我");await("profile actions appear"){label("反馈问题")!=null&&label("设置")!=null}
            val settings=android.graphics.Rect().also {label("设置")!!.getBoundsInScreen(it)};val report=android.graphics.Rect().also {label("反馈问题")!!.getBoundsInScreen(it)}
            assertTrue("Feedback belongs on the left edge",report.centerX()<context.resources.displayMetrics.widthPixels/4)
            assertTrue("Settings stays on the right edge",settings.centerX()>context.resources.displayMetrics.widthPixels*3/4)
            assertNull("The profile header has no Me title",node {n->val bounds=android.graphics.Rect().also(n::getBoundsInScreen);n.text?.toString()=="我"&&kotlin.math.abs(bounds.centerY()-report.centerY())<report.height()})
            shot("profile-envelope-header.png")
            click("设置");await("settings opens"){label("关于")!=null}
            for(text in listOf("通用","智能功能","数据","其他","主题与界面语言","正文样式、字号与阅读字体","服务地址、密钥与模型","模型与目标语言","WebDAV 书籍与阅读进度"))assertNull(text,label(text))
            shot("settings-home-simplified.png")
            click("云同步");await("cloud fields load"){node {it.text?.toString()?.contains("设备名称")==true}!=null}
            assertNull(label("清除已保存的密码"));assertNull(label("后台同步由系统调度，省电模式下可能延后"))
            assertNull(label("查看云盘配置说明"));assertNull(label("立即同步"))
            val device=android.graphics.Rect().also {node {it.text?.toString()?.contains("设备名称")==true}!!.getBoundsInScreen(it)}
            val password=android.graphics.Rect().also {node {it.text?.toString()?.contains("密码")==true}!!.getBoundsInScreen(it)}
            assertTrue("Device name follows password",device.top>=password.bottom);shot("cloud-settings-device-name.png")
            click("返回");await("settings returns"){label("关于")!=null}
            click("关于");await("about shows version status"){label("Torto")!=null&&label("当前已是最新版本")!=null}
            for(text in listOf("反馈问题","开源许可","开源项目","应用许可 · AGPL-3.0-only","导出诊断日志","更新来源","检查更新"))assertNull(text,label(text))
            shot("about-simplified.png")
            click("返回");await("settings returns from about"){label("关于")!=null}
            click("返回");await("profile returns"){label("反馈问题")!=null}
            click("反馈问题");await("feedback loads"){feedback!!.state.value.visible&&!feedback!!.state.value.loading};assertFalse(feedback!!.state.value.valid)
            // A blocked storage queue must never delay character counts or button feedback.
            val native=FeedbackModel::class.java.getDeclaredField("session").apply {isAccessible=true}.get(feedback) as org.tortotech.torto.core.MobileFeedback
            val stale=native.snapshot()
            val storage=FeedbackModel::class.java.getDeclaredField("operations").apply {isAccessible=true}.get(feedback) as kotlinx.coroutines.sync.Mutex
            storage.lock()
            try {
                instrumentation.runOnMainSync {
                    feedback!!.edit(description="一二三四五六七八九")
                    assertEquals(9u,feedback!!.state.value.count);assertFalse(feedback!!.state.value.valid)
                    feedback!!.edit(description="一二三四五六七八九十")
                    assertEquals(10u,feedback!!.state.value.count);assertTrue(feedback!!.state.value.valid)
                }
                fun submitEnabled()=clickable(label("提交"))?.isEnabled==true
                await("valid input enables Submit while storage is blocked"){submitEnabled()}
                android.os.SystemClock.sleep(700)
                instrumentation.runOnMainSync {
                    feedback!!.edit(description="一二三四五六七八九十一")
                    assertTrue("More valid input never disables submission",feedback!!.state.value.valid)
                    // Deliver the real session's older acknowledgement after the latest edit.
                    FeedbackModel::class.java.getDeclaredMethod("apply",org.tortotech.torto.core.FeedbackState::class.java,Boolean::class.javaPrimitiveType).apply {isAccessible=true}.invoke(feedback,stale,false)
                    assertEquals(11u,feedback!!.state.value.count);assertTrue(feedback!!.state.value.valid)
                    feedback!!.edit(description=" \n一二三四五六七八九\u200b")
                    assertEquals(9u,feedback!!.state.value.count);assertFalse("Deletion below ten disables immediately",feedback!!.state.value.valid)
                    feedback!!.edit(description="👨‍👩‍👧‍👦".repeat(10))
                    assertEquals(10u,feedback!!.state.value.count);assertTrue(feedback!!.state.value.valid)
                    feedback!!.edit(description="x".repeat(5001))
                    assertEquals(5001u,feedback!!.state.value.count);assertFalse(feedback!!.state.value.valid)
                }
            }finally {storage.unlock()}
            await("feedback text field appears"){node {it.isEditable&&!it.isPassword}!=null}
            val input=node {it.isEditable&&!it.isPassword};assertNotNull(input)
            val description="这是一次用于验证反馈草稿保存和恢复行为的测试说明"
            assertTrue(input!!.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT,Bundle().apply {putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,description)}))
            await("description validates and saves"){feedback!!.state.value.valid}
            val service=instrumentation.uiAutomation.serviceInfo
            service.flags=service.flags or android.accessibilityservice.AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS
            instrumentation.uiAutomation.serviceInfo=service
            await("feedback description focuses without tapping"){node {it.isEditable&&it.isFocused}!=null}
            val pane=node {it.paneTitle?.toString()=="反馈问题"}!!
            val bounds=android.graphics.Rect().also(pane::getBoundsInScreen)
            assertEquals("Feedback fills the top edge",0,bounds.top)
            assertEquals("Feedback fills the bottom edge",activity!!.window.decorView.height,bounds.bottom)
            fun keyboard()=instrumentation.uiAutomation.windows.firstOrNull {it.type==android.view.accessibility.AccessibilityWindowInfo.TYPE_INPUT_METHOD}
            await("feedback keyboard appears"){keyboard()!=null}
            android.os.SystemClock.sleep(300)
            val keyboardBounds=android.graphics.Rect().also {keyboard()!!.getBoundsInScreen(it)}
            val submitBounds=android.graphics.Rect().also {label("提交")!!.getBoundsInScreen(it)}
            assertTrue("Submitting remains above the keyboard",submitBounds.bottom<=keyboardBounds.top+2)
            shot("feedback-keyboard.png")
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("input keyevent 4")).use {it.readBytes()}
            await("keyboard closes without closing feedback"){keyboard()==null&&feedback!!.state.value.visible}
            click("附带信息");await("environment appears"){label("设备")!=null}
            assertEquals("shelf",org.json.JSONObject(feedback!!.state.value.context).getString("source"));shot("feedback-form-light.png")
            click("关闭反馈");await("feedback closes"){!feedback!!.state.value.visible}
            click("反馈问题");await("draft restores"){feedback!!.state.value.visible&&!feedback!!.state.value.loading&&feedback!!.state.value.description==description}
            click("关闭反馈");await("feedback closes again"){!feedback!!.state.value.visible}
            val fixture=File(context.cacheDir,"feedback-location.fb2");fixture.writeText("""<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>Feedback location fixture</book-title><lang>en</lang></title-info></description><body><section><title><p>Chapter one</p></title><p>Reading position remains unchanged when reporting a problem.</p><p>A second paragraph is available for focus navigation.</p></section></body></FictionBook>""")
            val book=core.importBook(fixture.absolutePath).first {it.title=="Feedback location fixture"};imported=book.id;fixture.delete()
            instrumentation.runOnMainSync {reader!!.open(book)};await("book opens"){val s=reader!!.state.value;if(s.error!=null)fail("Reader error: ${s.error}");s.frame!=null&&!s.busy}
            instrumentation.runOnMainSync {if(!reader!!.state.value.chromeVisible)reader!!.toggleChrome()};await("reader feedback action appears"){label("反馈问题")!=null}
            val frame=reader!!.state.value.frame!!
            click("反馈问题");await("reader context captures"){feedback!!.state.value.visible&&!feedback!!.state.value.loading}
            val payload=org.json.JSONObject(feedback!!.state.value.context)
            assertEquals("reader",payload.getString("source"));assertEquals(book.title,payload.getJSONObject("book").getString("title"))
            assertFalse(feedback!!.state.value.context.contains("apiKey")||feedback!!.state.value.context.contains("password"));assertEquals(description,feedback!!.state.value.description)
            click("关闭反馈");await("reader feedback closes"){!feedback!!.state.value.visible}
            assertEquals(frame.info.progress,reader!!.state.value.frame!!.info.progress,.000001);assertEquals(frame.info.section,reader!!.state.value.frame!!.info.section);assertEquals(frame.info.page,reader!!.state.value.frame!!.info.page)
        }finally {
            instrumentation.runOnMainSync {feedback?.close();reader?.back()};await("reader exits"){reader?.state?.value?.active==null&&reader?.state?.value?.busy!=true}
            core.saveAiOptions(originalAi);core.saveReadingOptions(originalReading)
            previous?.let {saved->instrumentation.runOnMainSync {reader!!.interfaceSettings(saved.appTheme,saved.language)};await("interface restores"){!reader!!.state.value.busy}}
            android.os.SystemClock.sleep(600);instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor)
            imported?.let {core.removeBook(it)}
            names.forEach {name->val file=File(directory,name);backup[name]?.let {file.writeBytes(it)} ?: file.delete()}
        }
    }
}

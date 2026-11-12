package org.tortotech.torto

import android.graphics.Bitmap
import android.view.accessibility.AccessibilityNodeInfo
import androidx.lifecycle.ViewModelProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.CompletableDeferred
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.tortotech.torto.core.AppUpdate
import java.io.File
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference

@RunWith(AndroidJUnit4::class)
class AboutUpdateTest {
    private val instrumentation get()=InstrumentationRegistry.getInstrumentation()
    private fun await(message:String,condition:()->Boolean){
        val deadline=android.os.SystemClock.uptimeMillis()+8000
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
    private fun click(value:String){
        fun target():AccessibilityNodeInfo?{
            var item=label(value)
            while(item!=null&&!item.isClickable)item=item.parent
            return item?.takeIf {it.isEnabled}
        }
        await("$value is actionable"){target()!=null}
        assertTrue(target()!!.performAction(AccessibilityNodeInfo.ACTION_CLICK))
    }
    private fun screenshot(name:String){
        instrumentation.waitForIdleSync();android.os.SystemClock.sleep(400)
        instrumentation.uiAutomation.takeScreenshot()?.let {bitmap->
            File(instrumentation.targetContext.cacheDir,name).outputStream().use {bitmap.compress(Bitmap.CompressFormat.PNG,100,it)}
            bitmap.recycle()
        }
    }
    @Test fun automaticChecksAreQuietCoalescedAndUseTheAutomaticSource(){
        val originalLoader=UpdateChecks.load
        val pending=CompletableDeferred<AppUpdate?>()
        val calls=AtomicInteger();val source=AtomicReference<String>();val version=AtomicReference<String>()
        val prefs=instrumentation.targetContext.getSharedPreferences("interface",0)
        val previousSource=prefs.getString("updateSource",null)
        prefs.edit().putString("updateSource","gitee").commit()
        val monitor=instrumentation.addMonitor(MainActivity::class.java.name,null,false)
        var activity:MainActivity?=null;var model:ReaderModel?=null;var previous:ScreenState?=null
        UpdateChecks.load={_,current,_,selected->
            source.set(selected);version.set(current)
            when(calls.incrementAndGet()){
                1->pending.await()
                2->throw IllegalStateException("offline fixture")
                else->AppUpdate("99.0.0","## Improvement\n\n- Better reading\n\n<details>\n<summary>中文更新说明</summary>\n\n## Improvement\n\n- 改进阅读体验\n\n</details>",1048576uL,"GitHub","test-token")
            }
        }
        try {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand("am start -W -f 0x10008000 -n org.tortotech.torto.debug/org.tortotech.torto.MainActivity")).use {it.readBytes()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {model=ViewModelProvider(activity!!)[ReaderModel::class.java]}
            await("startup checks without blocking the shelf"){model!!.state.value.loaded&&!model!!.state.value.busy&&model!!.state.value.checkingUpdate&&calls.get()==1}
            previous=model!!.state.value
            assertEquals("automatic",source.get());assertEquals(model!!.state.value.appVersion,version.get())
            assertFalse("The retired source preference is removed",prefs.contains("updateSource"))
            instrumentation.runOnMainSync {model!!.checkUpdatesIfNeeded();model!!.checkUpdates();model!!.interfaceSettings("light","zh-CN")}
            await("Chinese interface applies"){model!!.state.value.language=="zh-CN"&&!model!!.state.value.busy}
            assertEquals(1,calls.get())
            click("我");click("设置");click("关于")
            await("about shares the pending startup check"){label("正在检查更新")!=null&&label("Torto")!=null}
            assertEquals(1,calls.get())
            for(text in listOf("反馈问题","开源许可","开源项目","应用许可 · AGPL-3.0-only","导出诊断日志","更新来源","检查更新"))assertNull(text,label(text))
            pending.complete(null)
            await("the latest version status is shown"){model!!.state.value.updateChecked&&!model!!.state.value.checkingUpdate&&label("当前已是最新版本")!=null}
            screenshot("about-light.png")
            instrumentation.runOnMainSync {activity!!.recreate()}
            activity=monitor.waitForActivityWithTimeout(5000) as? MainActivity;assertNotNull(activity)
            instrumentation.runOnMainSync {assertSame(model,ViewModelProvider(activity!!)[ReaderModel::class.java])}
            click("设置");click("关于")
            await("reopening keeps the completed check"){label("当前已是最新版本")!=null}
            assertEquals("Activity recreation must not repeat discovery",1,calls.get())
            instrumentation.runOnMainSync {model!!.interfaceSettings("dark","zh-CN")}
            await("dark mode applies"){model!!.state.value.appTheme=="dark"&&!model!!.state.value.busy}
            screenshot("about-dark.png")
            instrumentation.runOnMainSync {model!!.checkUpdates()}
            await("failure stays local and can be retried"){label("暂时无法检查更新")!=null&&label("重试")!=null&&!model!!.state.value.checkingUpdate}
            assertNull(model!!.state.value.error)
            click("重试")
            await("retry discovers a new version"){model!!.state.value.appUpdate?.version=="99.0.0"&&node {it.text?.toString()?.startsWith("下载更新")==true}!=null}
            assertEquals(3,calls.get());assertEquals("automatic",source.get())
            assertFalse("Discovery must not download or install an update",model!!.state.value.downloadingUpdate)
            assertNull(model!!.state.value.downloadedUpdate)
            screenshot("about-update-available.png")
        }finally {
            pending.complete(null);UpdateChecks.load=originalLoader
            previous?.let {saved->instrumentation.runOnMainSync {model!!.interfaceSettings(saved.appTheme,saved.language)};await("interface restores"){!model!!.state.value.busy}}
            instrumentation.runOnMainSync {activity?.finish()};instrumentation.removeMonitor(monitor)
            prefs.edit().apply {if(previousSource==null)remove("updateSource")else putString("updateSource",previousSource)}.commit()
        }
    }
}

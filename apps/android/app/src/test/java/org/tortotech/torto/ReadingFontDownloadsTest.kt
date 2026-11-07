package org.tortotech.torto

import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import org.junit.Assert.*
import org.junit.Test
import org.tortotech.torto.core.ReadingFontDownload

class ReadingFontDownloadsTest {
    @Test fun startupAndManualRequestsShareOneBackgroundBatch():Unit=runBlocking {
        val scope=CoroutineScope(SupervisorJob()+Dispatchers.Default)
        try {
            val downloads=ReadingFontDownloads(scope)
            val started=CompletableDeferred<Unit>();val finish=CompletableDeferred<Unit>()
            val calls=java.util.concurrent.atomic.AtomicInteger()
            val load:suspend ()->ReadingFontDownload={calls.incrementAndGet();started.complete(Unit);finish.await();ReadingFontDownload(3u,true,null)}
            downloads.request(true,load)
            withTimeout(5000){started.await()}
            // Caller/view-model cancellation does not own the process job.
            val activity=launch {downloads.request(true,load)};activity.cancelAndJoin()
            repeat(8){downloads.request(it%2==0,load)}
            assertTrue(downloads.state.value.running)
            assertEquals(1,calls.get())
            finish.complete(Unit)
            val result=withTimeout(5000){downloads.state.first {!it.running}}
            assertTrue(result.ready);assertEquals(3u,result.downloaded)
            downloads.request(true,load)
            assertEquals(1,calls.get())
        }finally{scope.cancel()}
    }
    @Test fun failuresSettleWithoutBlockingAndManualRetryCanFillMissingFonts():Unit=runBlocking {
        val scope=CoroutineScope(SupervisorJob()+Dispatchers.Default)
        try {
            val downloads=ReadingFontDownloads(scope)
            downloads.request(true){ReadingFontDownload(1u,false,"offline")}
            val partial=withTimeout(5000){downloads.state.first {it.revision==1L}}
            assertFalse(partial.running);assertFalse(partial.ready);assertEquals("offline",partial.error)
            downloads.request(false){ReadingFontDownload(2u,true,null)}
            val done=withTimeout(5000){downloads.state.first {it.revision==2L}}
            assertTrue(done.ready);assertNull(done.error);assertEquals(2u,done.downloaded)
        }finally{scope.cancel()}
    }
}

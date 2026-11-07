package org.tortotech.torto

import android.os.Bundle
import androidx.test.runner.AndroidJUnitRunner
import org.tortotech.torto.core.ReadingFontDownload

/** Existing reader tests are offline; automatic font tests inject their own
 * controlled loader rather than relying on public endpoints or device fonts. */
class TortoTestRunner:AndroidJUnitRunner() {
    override fun onCreate(arguments:Bundle?) {
        ReadingFonts.loader={ReadingFontDownload(0u,false,null)}
        super.onCreate(arguments)
    }
}

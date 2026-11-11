package org.tortotech.torto

import java.util.Locale
import org.junit.Assert.*
import org.junit.Test

class ReadingLanguageTest {
    @Test fun sameLanguageIgnoresRegionAndMetadataCodeForm(){
        for(tag in listOf("en","en-GB"," EN_us ","eng"))
            assertFalse(tag,showReadingTranslation(listOf(tag),Locale.US,false))
        for(tag in listOf("zh-CN","zh-TW","zho","chi"))
            assertFalse(tag,showReadingTranslation(listOf(tag),Locale.CHINA,false))
        assertFalse(showReadingTranslation(listOf("deu","ger"),Locale.GERMAN,false))
    }
    @Test fun ForeignMissingAndMixedLanguagesKeepTranslationAvailable(){
        for(tags in listOf(emptyList(),listOf(""),listOf("und"),listOf("mul"),listOf("unknown"),listOf("zh-CN"),listOf("en","zh-CN"),listOf("en","")))
            assertTrue(tags.toString(),showReadingTranslation(tags,Locale.US,false))
        assertTrue(showReadingTranslation(listOf("en"),Locale.ROOT,false))
    }
    @Test fun matchingLanguageNeverHidesTheWayToTurnTranslationOff(){
        assertTrue(showReadingTranslation(listOf("en-US"),Locale.US,true))
    }
}

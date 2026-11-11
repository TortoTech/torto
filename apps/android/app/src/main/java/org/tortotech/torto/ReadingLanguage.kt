package org.tortotech.torto

import java.util.Locale
import java.util.MissingResourceException

/** Keep an enabled translation reachable so it can always be turned off. */
internal fun showReadingTranslation(bookLanguages:List<String>,system:Locale,translated:Boolean):Boolean {
    if(translated||bookLanguages.isEmpty())return true
    val systemLanguage=readingLanguage(system) ?: return true
    return bookLanguages.any {tag->
        readingLanguage(Locale.forLanguageTag(tag.trim().replace('_','-'))) != systemLanguage
    }
}

/** Compare language rather than region, including ISO 639 two- and three-letter codes. */
private fun readingLanguage(locale:Locale):String? {
    if(locale.language.isBlank()||locale.language in setOf("und","mul","zxx"))return null
    val language=try {locale.isO3Language} catch(_:MissingResourceException){return null}
    return when(language){
        "chi"->"zho";"fre"->"fra";"ger"->"deu";"dut"->"nld";"cze"->"ces"
        "gre"->"ell";"rum"->"ron";"slo"->"slk";"per"->"fas";"wel"->"cym"
        "alb"->"sqi";"arm"->"hye";"baq"->"eus";"bur"->"mya";"geo"->"kat"
        "ice"->"isl";"mac"->"mkd";"mao"->"mri";"may"->"msa";"tib"->"bod"
        else->language.takeIf {it.isNotBlank()}
    }
}

package org.tortotech.torto

import android.icu.util.ULocale
import java.util.Locale

private val wordLanguageAliases by lazy {Locale.getISOLanguages().associateBy {Locale.forLanguageTag(it).isO3Language}}

internal fun readingSelectionPreference(value:String?,focus:Boolean):String=
    value?.takeIf {it in setOf("free","word","sentence","paragraph")}?:if(focus)"paragraph"else"free"

internal fun effectiveReadingSelection(preference:String,focus:Boolean,words:Boolean):String=
    if(preference=="word"&&!words)readingSelectionPreference(null,focus)else readingSelectionPreference(preference,focus)

/** Primary authored language, including an explicit transliteration script. Unknown stays permissive. */
internal fun bookSupportsWordSelection(languages:List<String>):Boolean {
    for(tag in languages){
        val parsed=Locale.forLanguageTag(tag.trim().replace('_','-'))
        val language=readingLanguage(parsed) ?: continue
        val canonical=Locale.Builder().setLocale(parsed).setLanguage(wordLanguageAliases[language]?:parsed.language).build()
        val locale=ULocale.forLocale(canonical)
        val script=locale.script.ifBlank {ULocale.addLikelySubtags(locale).script}
        if(script.isNotBlank()&&script !in setOf("Zzzz","Zyyy","Zinh","Zxxx"))return script=="Latn"
    }
    return true
}

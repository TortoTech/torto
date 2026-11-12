package org.tortotech.torto

import org.junit.Assert.*
import org.junit.Test

class AssistantCitationTest {
    @Test fun inlineAndTableCitationsShareOneOrderedNumberingWithoutNumberingWebLinks(){
        fun run(text:String,link:String="")=AssistantRun(text,0,link,false)
        val blocks=listOf(
            AssistantBlock("paragraph",0,"",listOf(run("Text "),run("↗","link://j/2/first"),run("web","https://example.com")),emptyList()),
            AssistantBlock("table",0,"",emptyList(),listOf(listOf(listOf(run("↗","link://j/3/second"),run("↗","link://j/2/first"))))),
            AssistantBlock("code",0,"",listOf(run("link://j/3/second")),emptyList())
        )
        val numbered=numberAssistantCitations(blocks)
        assertEquals("①",numbered[0].runs[1].text)
        assertEquals("web",numbered[0].runs[2].text)
        assertEquals(listOf("②","①"),numbered[1].rows.single().single().map {it.text})
        assertEquals("link://j/3/second",numbered[2].runs.single().text)
        assertEquals("link://j/2/first",numbered[0].runs[1].link)
        assertEquals("㉔",assistantCitationMarker(24))
    }
}

package org.tortotech.torto

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp

@Composable
fun ServiceSettingsPanel(state: ScreenState, model: ReaderModel, section: String, openProviders:()->Unit={}) {
    if(section=="ai"){ProviderManager(state,model);return}
    if(section!="sync"){AiRoleSettings(state,model,section,openProviders);return}
    CloudSettingsPanel(state,model)
}

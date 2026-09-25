Pod::Spec.new do |s|
  s.name           = 'StenoLink'
  s.version        = '1.0.0'
  s.summary        = 'Bonjour discovery, pinned TLS requests and background chunk uploads for the Steno recorder'
  s.description    = 'Local Expo module: NWBrowser for _steno._tcp, URLSession with a leaf-certificate pin, background upload session, streaming SHA-256.'
  s.author         = 'Nicolai Schmid'
  s.homepage       = 'https://github.com/NicolaiSchmid/steno'
  s.license        = { :type => 'MIT' }
  # The podspec floor is the dev client's; the iOS 18.6 gate is at runtime
  # (plan decision 10, UpdateIOSScreen).
  s.platforms      = { :ios => '16.4' }
  s.swift_version  = '5.9'
  s.source         = { git: '' }
  s.static_framework = true

  s.dependency 'ExpoModulesCore'

  s.frameworks = 'Network', 'Security', 'CryptoKit'

  s.pod_target_xcconfig = {
    'DEFINES_MODULE' => 'YES',
    'SWIFT_COMPILATION_MODE' => 'wholemodule'
  }

  s.source_files = "**/*.{h,m,swift}"
end

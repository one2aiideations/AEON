################################################################################
#
# aeon-llama-cpp
#
################################################################################

AEON_LLAMA_CPP_VERSION = b12628
AEON_LLAMA_CPP_SITE = $(call github,ggml-org,llama.cpp,$(AEON_LLAMA_CPP_VERSION))
AEON_LLAMA_CPP_LICENSE = MIT
AEON_LLAMA_CPP_LICENSE_FILES = LICENSE

# Offline, portable CPU build (no host-specific instructions, no downloads at build time).
AEON_LLAMA_CPP_CONF_OPTS = \
	-DBUILD_SHARED_LIBS=ON \
	-DGGML_NATIVE=OFF \
	-DGGML_OPENMP=OFF \
	-DLLAMA_OPENSSL=OFF \
	-DLLAMA_BUILD_TESTS=OFF \
	-DLLAMA_BUILD_EXAMPLES=OFF \
	-DLLAMA_BUILD_TOOLS=ON \
	-DLLAMA_TOOLS_INSTALL=ON \
	-DLLAMA_BUILD_SERVER=ON \
	-DLLAMA_BUILD_UI=OFF \
	-DLLAMA_USE_PREBUILT_UI=OFF \
	-DLLAMA_BUILD_APP=OFF

$(eval $(cmake-package))

################################################################################
#
# mouli-llama-cpp
#
################################################################################

MOULI_LLAMA_CPP_VERSION = b12628
MOULI_LLAMA_CPP_SITE = $(call github,ggml-org,llama.cpp,$(MOULI_LLAMA_CPP_VERSION))
MOULI_LLAMA_CPP_LICENSE = MIT
MOULI_LLAMA_CPP_LICENSE_FILES = LICENSE

# Offline CPU build with no downloads at build time. Not tuned to the build machine (GGML_NATIVE
# off) but compiled for AVX2/FMA (x86-64-v3, Intel Haswell 2013 / AMD Excavator 2015 or newer):
# without these, inference is several times slower. Older CPUs get a warning from moulid.
MOULI_LLAMA_CPP_CONF_OPTS = \
	-DBUILD_SHARED_LIBS=ON \
	-DGGML_NATIVE=OFF \
	-DGGML_AVX2=ON \
	-DGGML_FMA=ON \
	-DGGML_F16C=ON \
	-DGGML_BMI2=ON \
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

cmake_minimum_required(VERSION 3.18)
project(tafrigh_cuda_link LANGUAGES C)

# transcribe-cpp-sys 0.4.1 exports the static ggml-cuda archive but omits its
# toolkit dependencies. Use CMake's toolkit discovery, including driver stubs
# on build hosts without a GPU; never add the stub directory to runtime paths.
find_package(CUDAToolkit REQUIRED)
set(library_dirs "")
foreach(component cudart cublas cublasLt cuda_driver)
    get_target_property(location CUDA::${component} IMPORTED_IMPLIB)
    if(NOT location)
        get_target_property(location CUDA::${component} IMPORTED_LOCATION)
    endif()
    if(NOT location)
        message(FATAL_ERROR "CUDA::${component} has no library location")
    endif()
    get_filename_component(directory "${location}" DIRECTORY)
    list(APPEND library_dirs "${directory}")
endforeach()
list(REMOVE_DUPLICATES library_dirs)
list(JOIN library_dirs "\n" library_dirs)
file(WRITE "${CMAKE_BINARY_DIR}/cuda-libraries.txt" "${library_dirs}\n")

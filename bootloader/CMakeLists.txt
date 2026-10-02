# Not an application: an ESP-IDF project that exists only to build the
# second-stage bootloader with rollback enabled (ADR-0024). dev/bootloader.sh
# builds it in Docker and copies the result to bootloader.bin.
cmake_minimum_required(VERSION 3.16)
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(cat_feeder_bootloader)

from setuptools import setup, find_packages

setup(
    name="agent_grid_protocol_core",
    version="0.2.0",
    description="Agent Grid Protocol SDK - Autonomous AI API Quota & Token Exchange Client",
    long_description="Agent Grid Protocol SDK - Autonomous AI API Quota & Token Exchange Client",
    long_description_content_type="text/plain",
    author="Agent Grid Protocol Team",
    url="https://github.com/agent-grid/protocol",
    py_modules=["agent_grid_client"],
    install_requires=[
        "requests>=2.20.0",
    ],
    classifiers=[
        "Programming Language :: Python :: 3",
        "License :: OSI Approved :: MIT License",
        "Operating System :: OS Independent",
    ],
    python_requires=">=3.7",
)

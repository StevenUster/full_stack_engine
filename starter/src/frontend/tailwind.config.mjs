/** @type {import('tailwindcss').Config} */
export default {
	content: ['./src/**/*.{astro,html,js,jsx,md,mdx,svelte,ts,tsx,vue}'],
	theme: {
		extend: {
			colors: {
				primary: {
					50: '#f0f4fa',
					100: '#dce8f7',
					200: '#b8d0f0',
					300: '#8ab0e6',
					400: '#528bd9',
					500: '#2663c9',
					600: '#00205c',
					700: '#001a4d',
					800: '#00143d',
					900: '#000e2e',
					950: '#00081f',
				},
				secondary: {
					50: '#fbfaf7',
					100: '#f6f4ed',
					200: '#d1caae',
					300: '#c4bb97',
					400: '#afa37e',
					500: '#8f825e',
					600: '#6f6345',
					700: '#514934',
					800: '#383224',
					900: '#231f16',
					950: '#13100b',
				}
			}
		},
	},
	plugins: [],
}
